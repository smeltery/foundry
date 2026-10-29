"""Validate the selected authored client formats, not arbitrary client extensions."""
import hashlib
import os
from pathlib import Path
import re

from files import contained, fields, read_json, require, text_value


DISCOVERY = ('.agents/plugins/marketplace.json', '.claude-plugin/marketplace.json',
             '.cursor-plugin/marketplace.json')
PACKAGE = 'plugins/foundry'
PLUGIN_SCHEMA = 'https://agent-plugins.org/schemas/1.0.0/plugin.schema.json'
MCP_SCHEMA = 'https://agent-plugins.org/schemas/1.0.0/mcp.schema.json'
CONFIGS = ('mcp.json', '.mcp.json', '.cursor-plugin/mcp.json')


def discovery(root):
    for name in DISCOVERY:
        document = read_json(contained(root, name))
        required = ('name', 'plugins') if name.startswith('.agents/') else ('name', 'owner', 'plugins')
        fields(document, required)
        require(document['name'] == 'foundry', 'marketplace identity')
        if 'owner' in document:
            fields(document['owner'], ('name',))
            text_value(document['owner']['name'])
        entries = document['plugins']
        require(isinstance(entries, list) and len(entries) == 1, 'one package required')
        entry = entries[0]
        fields(entry, ('name', 'source'))
        require(entry['name'] == 'foundry', 'discovery package identity')
        source = entry['source']
        if name.startswith('.agents/'):
            fields(source, ('source', 'path'))
            require(source['source'] == 'local', 'local discovery required')
            source = source['path']
        require(source == './' + PACKAGE, 'discovery path mismatch')
        contained(root, source, directory=True)


def component(package, value, expected):
    require(value == expected, 'component path mismatch')
    return contained(package, value, directory=value.endswith('/'))


def descriptors(package, prepared):
    portable = read_json(contained(package, 'plugin.json'))
    common = ('name', 'version', 'description', 'author')
    fields(portable, ('$schema', *common), ())
    require(portable['$schema'] == PLUGIN_SCHEMA, 'portable schema identity')
    version = text_value(portable['version'])
    require(re.fullmatch(r'(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?', version),
            'invalid package version')
    documents = [portable]
    for client in ('claude', 'codex', 'cursor'):
        document = read_json(contained(package, f'.{client}-plugin/plugin.json'))
        fields(document, common, ('skills', 'commands', 'rules', 'mcpServers',
                                 'logo', 'icon'))
        require(document['version'] == version, 'client version mismatch')
        if client == 'cursor':
            for key in ('commands', 'skills', 'rules'):
                require(key in document, 'missing client component')
                component(package, document[key], f'./{key}/')
            if prepared:
                require(document.get('mcpServers') == './.cursor-plugin/mcp.json',
                        'Cursor MCP override missing')
            else:
                require('mcpServers' not in document, 'unprepared connection declaration')
        elif client == 'codex':
            component(package, document.get('skills'), './skills/')
        if prepared and client in ('claude', 'codex'):
            require(document.get('mcpServers') == './.mcp.json', 'native MCP override missing')
        if 'mcpServers' in document and client != 'cursor':
            require(prepared, 'unprepared connection declaration')
            component(package, document['mcpServers'], './.mcp.json')
        for key in ('skills', 'commands', 'rules'):
            if key in document:
                component(package, document[key], f'./{key}/')
        documents.append(document)
    for document in documents:
        require(document['name'] == 'foundry', 'package identity')
        text_value(document['description'])
        fields(document['author'], ('name',), ('email', 'url'))
        text_value(document['author']['name'])
        if 'keywords' in document:
            require(isinstance(document['keywords'], list), 'keyword list required')
            for keyword in document['keywords']:
                text_value(keyword)
    return documents


def connections(root, package, prepared):
    if not prepared:
        require(not (root / 'prepared.json').exists(), 'prepared marker in source mode')
        require(not (package / 'bin').exists(), 'binary in source package')
        require(all(not (package / name).exists() for name in CONFIGS),
                'source package must not claim a configured connection')
        return
    receipt = read_json(contained(root, 'prepared.json'))
    fields(receipt, ('version', 'executable_sha256', 'database'))
    require(type(receipt['version']) is int and receipt['version'] == 1, 'preparation version')
    binary = contained(package, './bin/f5y')
    require(os.access(binary, os.X_OK), 'copied binary is not executable')
    require(receipt['executable_sha256'] == hashlib.sha256(binary.read_bytes()).hexdigest(),
            'copied binary digest mismatch')
    database = text_value(receipt['database'])
    require(Path(database).is_absolute() and Path(database).is_file() and
            not Path(database).is_symlink(), 'selected database must exist')
    for name in CONFIGS:
        document = read_json(contained(package, name))
        fields(document, ('$schema', 'mcpServers') if name == 'mcp.json' else ('mcpServers',))
        if name == 'mcp.json':
            require(document['$schema'] == MCP_SCHEMA, 'portable MCP schema')
        fields(document['mcpServers'], ('foundry',))
        server = document['mcpServers']['foundry']
        typed = name != '.cursor-plugin/mcp.json'
        fields(server, ('type', 'command', 'args') if typed else ('command', 'args'))
        if typed:
            require(server['type'] == 'stdio', 'local stdio required')
        expected_command = './bin/f5y' if name == 'mcp.json' else str(binary)
        require(server['command'] == expected_command, 'selected binary path mismatch')
        require(server['args'] == ['--database', database, 'mcp'], 'selected database argv mismatch')
