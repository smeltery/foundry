"""Official SDK subprocess interoperability tests; no provider or client settings."""
import argparse
import asyncio
from datetime import datetime, timezone
import hashlib
import importlib.metadata
import json
from pathlib import Path
import platform
import sqlite3
import sys
import tempfile

from jsonschema import Draft202012Validator
from mcp import Client, MCPError, StdioServerParameters
from mcp.types import RequestParamsMeta


async def exercise(binary, report):
    def record(name, value):
        report['checks'].append({'check': name, 'value': value})

    with tempfile.TemporaryDirectory(prefix='foundry-mcp-sdk-') as directory:
        database = Path(directory) / 'sessions.db'
        parameters = StdioServerParameters(
            command=str(binary), args=['mcp', '--database', str(database)], cwd=directory,
        )
        async with Client(parameters, read_timeout_seconds=10) as client:
            assert client.protocol_version == '2025-11-25'
            record('initialize', {
                'protocol': client.protocol_version,
                'server': client.server_info.model_dump(mode='json'),
                'capabilities': client.server_capabilities.model_dump(mode='json'),
            })
            await client.session.send_ping()
            record('ping', 'passed')
            listed = await client.list_tools()
            schemas = {tool.name: tool.input_schema for tool in listed.tools}
            assert set(schemas) == {
                'session_create', 'session_list', 'session_get', 'session_send', 'session_cancel',
            }
            for schema in schemas.values():
                Draft202012Validator.check_schema(schema)
            record('schemas', sorted(schemas))

            async def call(name, arguments, *, error=False, **options):
                Draft202012Validator(schemas[name]).validate(arguments)
                result = await client.call_tool(name, arguments, **options)
                assert result.is_error is error, result
                assert len(result.content) == 1 and result.content[0].type == 'text'
                value = json.loads(result.content[0].text)
                record(name, {'arguments': arguments, 'is_error': result.is_error, 'data': value})
                return value

            create_args = {
                'prompt': 'Synthetic SDK fixture; do not execute', 'directory': directory,
                'idempotency_key': 'sdk-create',
            }
            created = await call('session_create', create_args)
            session_id = created['session']['id']
            assert created['session']['lifecycle'] == 'queued'
            assert created['execution'] == 'not_started'
            assert created['session']['active_attempt_id'] is None
            assert await call('session_create', create_args) == created
            sessions = await call('session_list', {})
            assert [session['id'] for session in sessions['sessions']] == [session_id]
            assert (await call('session_get', {'session_id': session_id}))['session'] == created['session']
            send_args = {
                'session_id': session_id, 'message_id': 'sdk-message-1', 'message': 'Synthetic follow-up',
            }
            sent = await call('session_send', send_args)
            assert sent['message']['state'] == 'pending'
            assert sent['delivery_not_confirmed'] is True
            assert sent['completion'] == 'not_confirmed'
            assert await call('session_send', send_args) == sent
            conflict = await call('session_send', {**send_args, 'message': 'Changed'}, error=True)
            assert conflict['code'] == 'conflict'
            cancelled = await call('session_cancel', {'session_id': session_id, 'mode': 'graceful'})
            assert cancelled['session']['lifecycle'] == 'suspended'
            assert cancelled['cancellation'] == 'requested'
            stopped = await call('session_send', {**send_args, 'message_id': 'sdk-message-2'})
            assert stopped['message']['state'] == 'pending'
            fetched = await call('session_get', {'session_id': session_id})
            assert fetched['session']['lifecycle'] == 'suspended'
            assert fetched['session']['active_attempt_id'] is None
            assert fetched['session']['outcome'] is None

            sessions = await call('session_list', {})
            meta = RequestParamsMeta(progress_token='sdk-progress')
            with_meta = await client.list_tools(meta=meta, cache_mode='bypass')
            assert [tool.name for tool in with_meta.tools] == [tool.name for tool in listed.tools]
            record('metadata_list', 'passed')
            assert await call('session_list', {}, meta=meta) == sessions
            opaque = {'com.example/audit': {'nested': [True, None, 7]}}
            assert await call('session_list', {}, meta=opaque) == sessions
            progress = []

            async def on_progress(*values):
                progress.append(values)

            assert await call('session_list', {}, progress_callback=on_progress) == sessions
            assert not progress, 'This server does not advertise or fabricate progress'
            record('optional_progress_callback', 'accepted without notifications')
            for name, args in [('session_send', {}), ('unknown_tool', {})]:
                try:
                    await client.call_tool(name, args)
                except MCPError as error:
                    assert error.code == -32602
                    record('invalid_request', {'tool': name, 'code': error.code})
                else:
                    raise AssertionError(f'{name}: expected invalid-params error')

        async with Client(parameters, read_timeout_seconds=10) as client:
            result = await client.call_tool('session_get', {'session_id': session_id})
            reopened = json.loads(result.content[0].text)
            assert not result.is_error and reopened['session']['lifecycle'] == 'suspended'
            record('reconnect_persistence', reopened)
        with sqlite3.connect(f'file:{database}?mode=ro', uri=True) as connection:
            rows = connection.execute('SELECT id,sequence,state FROM messages ORDER BY sequence').fetchall()
            assert [row[0] for row in rows] == ['sdk-message-1', 'sdk-message-2']
            assert all(row[2] == 'pending' for row in rows)
            record('message_persistence', rows)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--report', type=Path, help='Optional JSON evidence artifact')
    args = parser.parse_args()
    if not __debug__:
        parser.error('Run without -O; this test harness requires assertions')
    binary = args.binary.resolve(strict=True)
    for package in ('mcp', 'mcp-types'):
        if importlib.metadata.version(package) != '2.3.0':
            parser.error(f'{package} must match the pinned version 2.3.0')
    report = {
        'started_utc': datetime.now(timezone.utc).isoformat(),
        'sdk_version': importlib.metadata.version('mcp'),
        'python': sys.version, 'platform': platform.platform(),
        'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(), 'checks': [],
    }
    try:
        asyncio.run(asyncio.wait_for(exercise(binary, report), timeout=45))
        report['status'] = 'passed'
    except Exception as error:
        report['status'] = 'failed'
        report['error'] = {'type': type(error).__name__, 'message': str(error)}
        raise
    finally:
        report['finished_utc'] = datetime.now(timezone.utc).isoformat()
        if args.report:
            args.report.write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps({'status': report['status'], 'checks': len(report['checks'])}))


if __name__ == '__main__':
    main()
