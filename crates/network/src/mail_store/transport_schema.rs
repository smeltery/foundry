pub(super) const OUTBOX: &str = "CREATE TABLE transport_outbox(id TEXT PRIMARY KEY NOT NULL,team TEXT NOT NULL,peer BLOB NOT NULL CHECK(length(peer)=32),payload BLOB NOT NULL,created_ms INTEGER NOT NULL CHECK(created_ms>=0),revision INTEGER NOT NULL UNIQUE CHECK(revision>0),attempt INTEGER,generation BLOB,CHECK((attempt IS NULL AND generation IS NULL) OR (attempt IS NOT NULL AND length(generation)=8)))";
pub(super) const MEMBERS: &str = "CREATE TABLE remote_members(peer BLOB PRIMARY KEY NOT NULL CHECK(length(peer)=32),person TEXT NOT NULL,machine TEXT NOT NULL)";
pub(super) const PRESENCE: &str = "CREATE TABLE remote_presence(peer BLOB PRIMARY KEY NOT NULL CHECK(length(peer)=32),team TEXT NOT NULL,generation BLOB NOT NULL CHECK(length(generation)=8),payload BLOB NOT NULL,online INTEGER NOT NULL CHECK(online IN (0,1)))";
pub(super) const EPOCH: &str = "CREATE TABLE remote_epoch(id INTEGER PRIMARY KEY CHECK(id=1),team TEXT NOT NULL,revision BLOB NOT NULL CHECK(length(revision)=8))";

pub(super) fn tables() -> [(&'static str, &'static str); 4] {
    [
        ("transport_outbox", OUTBOX),
        ("remote_members", MEMBERS),
        ("remote_presence", PRESENCE),
        ("remote_epoch", EPOCH),
    ]
}
