-- Schema v11 database written by the v11 store (origin/main fcb3451) through its public API.
-- Dumped with sqlite3 .dump; regenerate only from a v11 build, never from a newer store.
PRAGMA foreign_keys=OFF;
BEGIN TRANSACTION;
CREATE TABLE sessions (
                id TEXT PRIMARY KEY NOT NULL,
                idempotency_key TEXT UNIQUE,
                creation TEXT NOT NULL,
                body TEXT NOT NULL
            );
INSERT INTO sessions VALUES('54b004cc-b044-4cfd-9a4b-7c88a3ab7bdc','fixture-key-1','{"prompt":"Summarize the sample notes.\nThen list open items.","directory":"/workspace/sample","model":{"provider":"local","model":"deterministic","effort":"low"},"idempotency_key":"fixture-key-1"}','{"id":"54b004cc-b044-4cfd-9a4b-7c88a3ab7bdc","prompt":"Summarize the sample notes.\nThen list open items.","directory":"/workspace/sample","model":{"provider":"local","model":"deterministic","effort":"low"},"lifecycle":"running","outcome":null,"result":null,"active_attempt_id":"648b25b9-f347-4f21-afbf-82b37c4e889a","version":1}');
INSERT INTO sessions VALUES('b7213685-ed29-4eaa-bf8e-c79f17c7b050','fixture-key-2','{"prompt":"Draft a plan.","directory":"/workspace/sample","model":null,"idempotency_key":"fixture-key-2"}','{"id":"b7213685-ed29-4eaa-bf8e-c79f17c7b050","prompt":"Draft a plan.","directory":"/workspace/sample","model":null,"lifecycle":"queued","outcome":null,"result":null,"active_attempt_id":null,"version":0}');
INSERT INTO sessions VALUES('519757b5-24eb-4e9c-93cf-f7387e1edf08',NULL,'{"prompt":"Check the sample.","directory":"/workspace/other","model":null,"idempotency_key":null}','{"id":"519757b5-24eb-4e9c-93cf-f7387e1edf08","prompt":"Check the sample.","directory":"/workspace/other","model":null,"lifecycle":"exit","outcome":"success","result":"checked","active_attempt_id":null,"version":2}');
CREATE TABLE attempts (
                id TEXT PRIMARY KEY NOT NULL,
                session_id TEXT NOT NULL REFERENCES sessions(id),
                token TEXT NOT NULL UNIQUE,
                expires_ms INTEGER NOT NULL,
                active INTEGER NOT NULL CHECK(active IN (0,1)),
                cancel TEXT CHECK(cancel IN ('graceful','hard'))
            );
INSERT INTO attempts VALUES('e4ef681d-91e2-4bb8-85bd-ba8eef4ffc0a','519757b5-24eb-4e9c-93cf-f7387e1edf08','d56a28e9-6403-41b5-b233-1fc63669193b',4947400962228,0,NULL);
INSERT INTO attempts VALUES('648b25b9-f347-4f21-afbf-82b37c4e889a','54b004cc-b044-4cfd-9a4b-7c88a3ab7bdc','b4210a18-df5e-4df4-babe-9f72e7a22573',4947400962229,1,NULL);
CREATE TABLE events (
                session_id TEXT NOT NULL REFERENCES sessions(id),
                sequence INTEGER NOT NULL CHECK(sequence>0),
                id TEXT NOT NULL,
                attempt_id TEXT NOT NULL REFERENCES attempts(id),
                revision INTEGER NOT NULL CHECK(revision>=0),
                kind TEXT NOT NULL,
                data TEXT NOT NULL,
                PRIMARY KEY(session_id,sequence),
                UNIQUE(session_id,id,revision)
            );
INSERT INTO events VALUES('519757b5-24eb-4e9c-93cf-f7387e1edf08',1,'s1','e4ef681d-91e2-4bb8-85bd-ba8eef4ffc0a',0,'text','{"text":"ok"}');
INSERT INTO events VALUES('54b004cc-b044-4cfd-9a4b-7c88a3ab7bdc',1,'e1','648b25b9-f347-4f21-afbf-82b37c4e889a',0,'text','{"text":"Part"}');
INSERT INTO events VALUES('54b004cc-b044-4cfd-9a4b-7c88a3ab7bdc',2,'e2','648b25b9-f347-4f21-afbf-82b37c4e889a',0,'tool_request','{"call_id":"c1"}');
INSERT INTO events VALUES('54b004cc-b044-4cfd-9a4b-7c88a3ab7bdc',3,'e1','648b25b9-f347-4f21-afbf-82b37c4e889a',1,'text','{"text":"Partial reply"}');
CREATE TABLE messages (
                session_id TEXT NOT NULL REFERENCES sessions(id),
                id TEXT NOT NULL,
                sequence INTEGER NOT NULL CHECK(sequence>0),
                text TEXT NOT NULL,
                state TEXT NOT NULL CHECK(state IN ('pending','in_flight','delivered','withdrawn')),
                attempt_id TEXT REFERENCES attempts(id),
                PRIMARY KEY(session_id,id),
                UNIQUE(session_id,sequence)
            );
INSERT INTO messages VALUES('54b004cc-b044-4cfd-9a4b-7c88a3ab7bdc','m1',1,'first follow-up','delivered','648b25b9-f347-4f21-afbf-82b37c4e889a');
INSERT INTO messages VALUES('54b004cc-b044-4cfd-9a4b-7c88a3ab7bdc','m2',2,'withdrawn follow-up','withdrawn',NULL);
INSERT INTO messages VALUES('54b004cc-b044-4cfd-9a4b-7c88a3ab7bdc','m3',3,'pending follow-up','pending',NULL);
CREATE TABLE tool_calls (
                session_id TEXT NOT NULL REFERENCES sessions(id),
                call_id TEXT NOT NULL,
                attempt_id TEXT NOT NULL REFERENCES attempts(id),
                tool TEXT NOT NULL,
                input_digest BLOB NOT NULL CHECK(length(input_digest)=32),
                outcome TEXT CHECK(outcome IN ('success','failed','uncertain')),
                output_digest BLOB CHECK(output_digest IS NULL OR length(output_digest)=32),
                PRIMARY KEY(session_id,call_id),
                CHECK(outcome!='success' OR output_digest IS NOT NULL)
            );
INSERT INTO tool_calls VALUES('54b004cc-b044-4cfd-9a4b-7c88a3ab7bdc','c1','648b25b9-f347-4f21-afbf-82b37c4e889a','read',x'0707070707070707070707070707070707070707070707070707070707070707','success',x'0808080808080808080808080808080808080808080808080808080808080808');
CREATE TABLE session_work (
                session_id TEXT PRIMARY KEY NOT NULL REFERENCES sessions(id),
                revision INTEGER NOT NULL DEFAULT 0
                    CHECK(typeof(revision)='integer' AND revision>=0)
            );
INSERT INTO session_work VALUES('54b004cc-b044-4cfd-9a4b-7c88a3ab7bdc',8);
INSERT INTO session_work VALUES('b7213685-ed29-4eaa-bf8e-c79f17c7b050',0);
INSERT INTO session_work VALUES('519757b5-24eb-4e9c-93cf-f7387e1edf08',2);
CREATE TABLE context_checkpoints (
                id TEXT PRIMARY KEY NOT NULL CHECK(typeof(id)='text' AND length(CAST(id AS BLOB))=36),
                session_id TEXT NOT NULL REFERENCES sessions(id) CHECK(typeof(session_id)='text' AND length(CAST(session_id AS BLOB))=36),
                attempt_id TEXT NOT NULL REFERENCES attempts(id) CHECK(typeof(attempt_id)='text' AND length(CAST(attempt_id AS BLOB))=36),
                generation INTEGER NOT NULL CHECK(typeof(generation)='integer' AND generation>0),
                parent_id TEXT CHECK(parent_id IS NULL OR (typeof(parent_id)='text' AND length(CAST(parent_id AS BLOB))=36)),
                request_id TEXT NOT NULL CHECK(typeof(request_id)='text' AND length(CAST(request_id AS BLOB)) BETWEEN 1 AND 128),
                metadata TEXT NOT NULL CHECK(length(CAST(metadata AS BLOB)) BETWEEN 1 AND 16384),
                coverage TEXT NOT NULL CHECK(length(CAST(coverage AS BLOB)) BETWEEN 1 AND 65536),
                payload BLOB NOT NULL CHECK(typeof(payload)='blob' AND length(payload) BETWEEN 1 AND 1048576),
                payload_digest BLOB NOT NULL CHECK(length(payload_digest)=32),
                request_digest BLOB NOT NULL CHECK(length(request_digest)=32),
                record_digest BLOB NOT NULL CHECK(length(record_digest)=32),
                configuration_digest BLOB NOT NULL CHECK(length(configuration_digest)=32),
                committed_ms INTEGER NOT NULL, request_format INTEGER NOT NULL DEFAULT 1
                CHECK(typeof(request_format)='integer' AND request_format IN (1,2)),
                UNIQUE(session_id,id), UNIQUE(session_id,generation), UNIQUE(session_id,request_id),
                FOREIGN KEY(session_id,parent_id) REFERENCES context_checkpoints(session_id,id)
            );
CREATE TABLE context_checkpoint_heads (
                session_id TEXT PRIMARY KEY NOT NULL REFERENCES sessions(id),
                checkpoint_id TEXT NOT NULL CHECK(typeof(checkpoint_id)='text' AND length(CAST(checkpoint_id AS BLOB))=36),
                FOREIGN KEY(session_id,checkpoint_id) REFERENCES context_checkpoints(session_id,id)
            );
CREATE TABLE context_finalizations (
                checkpoint_id TEXT PRIMARY KEY NOT NULL REFERENCES context_checkpoints(id)
                    CHECK(typeof(checkpoint_id)='text' AND length(CAST(checkpoint_id AS BLOB))=36),
                session_version INTEGER NOT NULL CHECK(typeof(session_version)='integer' AND session_version>=0),
                receipt_digest BLOB NOT NULL CHECK(typeof(receipt_digest)='blob' AND length(receipt_digest)=32)
            );
CREATE TABLE context_restores (
                id TEXT PRIMARY KEY NOT NULL CHECK(typeof(id)='text' AND length(CAST(id AS BLOB))=36),
                session_id TEXT NOT NULL REFERENCES sessions(id) CHECK(length(CAST(session_id AS BLOB))=36),
                checkpoint_id TEXT NOT NULL CHECK(length(CAST(checkpoint_id AS BLOB))=36),
                checkpoint_request_digest BLOB NOT NULL CHECK(length(checkpoint_request_digest)=32),
                configuration_digest BLOB NOT NULL CHECK(length(configuration_digest)=32),
                request_id TEXT NOT NULL CHECK(length(CAST(request_id AS BLOB)) BETWEEN 1 AND 128),
                begin_digest BLOB NOT NULL CHECK(length(begin_digest)=32),
                expected_session_version INTEGER NOT NULL CHECK(typeof(expected_session_version)='integer' AND expected_session_version>=0),
                token TEXT UNIQUE NOT NULL CHECK(length(CAST(token AS BLOB))=36),
                expires_ms INTEGER NOT NULL CHECK(typeof(expires_ms)='integer' AND expires_ms>0),
                cancel TEXT CHECK(cancel IN ('graceful','hard')),
                phase TEXT NOT NULL CHECK(phase IN ('loading','restored','continuing','closed','failed','cancelled','finished','expired')),
                continue_request_id TEXT CHECK(continue_request_id IS NULL OR length(CAST(continue_request_id AS BLOB)) BETWEEN 1 AND 128),
                continue_digest BLOB CHECK(continue_digest IS NULL OR length(continue_digest)=32),
                delivery_digest BLOB CHECK(delivery_digest IS NULL OR length(delivery_digest)=32),
                attempt_id TEXT UNIQUE REFERENCES attempts(id) CHECK(attempt_id IS NULL OR length(CAST(attempt_id AS BLOB))=36),
                admitted_session_version INTEGER CHECK(admitted_session_version IS NULL OR (typeof(admitted_session_version)='integer' AND admitted_session_version>=0)),
                admitted_work_revision INTEGER CHECK(admitted_work_revision IS NULL OR (typeof(admitted_work_revision)='integer' AND admitted_work_revision>=0)),
                receipt_digest BLOB CHECK(receipt_digest IS NULL OR length(receipt_digest)=32),
                UNIQUE(session_id,request_id),
                FOREIGN KEY(session_id,checkpoint_id) REFERENCES context_checkpoints(session_id,id),
                CHECK((attempt_id IS NULL AND continue_request_id IS NULL AND continue_digest IS NULL
                    AND delivery_digest IS NULL AND admitted_session_version IS NULL
                    AND admitted_work_revision IS NULL AND receipt_digest IS NULL)
                  OR (attempt_id IS NOT NULL AND continue_request_id IS NOT NULL AND continue_digest IS NOT NULL
                    AND delivery_digest IS NOT NULL AND admitted_session_version IS NOT NULL
                    AND admitted_work_revision IS NOT NULL AND receipt_digest IS NOT NULL)),
                CHECK((phase IN ('loading','restored','closed','failed','cancelled') AND attempt_id IS NULL)
                    OR (phase IN ('continuing','finished') AND attempt_id IS NOT NULL) OR phase='expired')
            );
CREATE TABLE context_continue_messages (
                restore_id TEXT PRIMARY KEY NOT NULL CHECK(typeof(restore_id)='text' AND length(CAST(restore_id AS BLOB))=36),
                session_id TEXT NOT NULL CHECK(typeof(session_id)='text' AND length(CAST(session_id AS BLOB))=36),
                message_id TEXT NOT NULL CHECK(typeof(message_id)='text' AND length(CAST(message_id AS BLOB)) BETWEEN 1 AND 128),
                sequence INTEGER NOT NULL CHECK(typeof(sequence)='integer' AND sequence>0),
                text_digest BLOB NOT NULL CHECK(typeof(text_digest)='blob' AND length(text_digest)=32),
                text_bytes INTEGER NOT NULL CHECK(typeof(text_bytes)='integer' AND text_bytes BETWEEN 1 AND 262144),
                FOREIGN KEY(session_id,restore_id) REFERENCES context_restores(session_id,id),
                FOREIGN KEY(session_id,message_id) REFERENCES messages(session_id,id)
            );
CREATE TABLE context_witnesses (
                checkpoint_id TEXT PRIMARY KEY NOT NULL CHECK(typeof(checkpoint_id)='text' AND length(CAST(checkpoint_id AS BLOB))=36),
                session_id TEXT NOT NULL CHECK(typeof(session_id)='text' AND length(CAST(session_id AS BLOB))=36),
                format INTEGER NOT NULL CHECK(typeof(format)='integer' AND format=1),
                body BLOB NOT NULL CHECK(typeof(body)='blob' AND length(body) BETWEEN 1 AND 262144),
                body_digest BLOB NOT NULL CHECK(typeof(body_digest)='blob' AND length(body_digest)=32),
                checkpoint_generation INTEGER NOT NULL CHECK(typeof(checkpoint_generation)='integer' AND checkpoint_generation>0),
                checkpoint_record_digest BLOB NOT NULL CHECK(typeof(checkpoint_record_digest)='blob' AND length(checkpoint_record_digest)=32),
                association_digest BLOB NOT NULL CHECK(typeof(association_digest)='blob' AND length(association_digest)=32),
                FOREIGN KEY(session_id,checkpoint_id) REFERENCES context_checkpoints(session_id,id)
             );
CREATE TABLE context_continue_acceptances (
                restore_id TEXT PRIMARY KEY NOT NULL REFERENCES context_restores(id)
                    CHECK(typeof(restore_id)='text' AND length(CAST(restore_id AS BLOB))=36),
                intent_body BLOB NOT NULL CHECK(typeof(intent_body)='blob' AND length(intent_body) BETWEEN 1 AND 4096),
                request_digest BLOB NOT NULL CHECK(typeof(request_digest)='blob' AND length(request_digest)=32),
                event_sequence INTEGER NOT NULL CHECK(typeof(event_sequence)='integer' AND event_sequence>0),
                committed_ms INTEGER NOT NULL CHECK(typeof(committed_ms)='integer' AND committed_ms>=0),
                record_digest BLOB NOT NULL CHECK(typeof(record_digest)='blob' AND length(record_digest)=32)
            );
CREATE UNIQUE INDEX active_attempt ON attempts(session_id) WHERE active=1;
CREATE UNIQUE INDEX loading_restore ON context_restores(session_id) WHERE phase IN ('loading','restored');
CREATE UNIQUE INDEX restore_session_identity ON context_restores(session_id,id);
COMMIT;
PRAGMA user_version=11;
