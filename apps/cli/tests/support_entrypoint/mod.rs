use std::{io::Read, process::Child};

/// Kill and wait on every child, returning their collected stderr. Failure
/// paths call this before panicking so no child outlives the test.
pub fn reap(children: &mut [&mut Child]) -> String {
    let mut stderr = String::new();
    for child in children {
        let _ = child.kill();
        let _ = child.wait();
        if let Some(mut pipe) = child.stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }
    }
    stderr
}
