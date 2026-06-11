//! Port allocation (spec/RUNTIME.md §8.2): random localhost port, never
//! fixed — bind `127.0.0.1:0`, read the assigned port, drop the listener,
//! pass the port to the child. The bind race is accepted; a child bind
//! failure is a spawn failure and Restarting picks a new port. Ports are
//! never written to config; connectors get them from the supervisor in
//! memory (`EndpointCell`).

use std::net::TcpListener;

pub fn allocate_port() -> std::io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocates_nonzero_loopback_ports() {
        let a = allocate_port().unwrap();
        let b = allocate_port().unwrap();
        assert_ne!(a, 0);
        assert_ne!(b, 0);
        // The port is free again after the listener drops: a child can
        // bind it (the §8.2 handoff).
        let rebind = TcpListener::bind(("127.0.0.1", a));
        assert!(rebind.is_ok(), "allocated port is handed off free");
    }
}
