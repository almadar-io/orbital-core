use super::*;
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Instant;
use {
    crate::os_host::OsHandles,
    orbital_core_base::os_grants::{NetGrant, OsGrants},
};

fn s(text: &str) -> Value {
    Value::string(text.to_string())
}

fn n(v: i64) -> Value {
    Value::Number(v as f64)
}

fn grants(entries: &[(String, &[NetKind])]) -> OsGrants {
    OsGrants {
        net: entries
            .iter()
            .map(|(target, kinds)| NetGrant {
                target: target.clone(),
                kinds: kinds.to_vec(),
            })
            .collect(),
        ..OsGrants::default()
    }
}

fn with_host<R>(grants: OsGrants, check: impl FnOnce(&OsHost<'_>) -> R) -> R {
    let handles = OsHandles::default();
    let on_event = |_: Option<&CallSite>, _: Value| {};
    check(&OsHost {
        grants: &grants,
        handles: &handles,
        on_event: &on_event,
        secrets: None,
    })
}

fn ask(host: &OsHost<'_>, op: OsOp, args: &[Value]) -> Result<Value, OsError> {
    answer(op, args, None, None, host).expect("net answers this op")
}

fn field(value: &Value, name: &str) -> Value {
    value
        .as_object()
        .and_then(|m| m.get(name))
        .cloned()
        .expect("field")
}

fn kind_of(result: Result<Value, OsError>) -> OsErrorKind {
    result.expect_err("an error").kind
}

fn tcp_grant(port: u16) -> OsGrants {
    grants(&[(format!("127.0.0.1:{port}"), &[NetKind::Tcp])])
}

#[test]
fn a_keyed_recv_under_a_route_defers_and_delivers_the_bytes_when_they_arrive() {
    use crate::os_host::completion::OsCompletionSink;
    use crate::resumable::CallRoutes;
    use std::sync::{Arc, Mutex};
    struct Collect(Mutex<mpsc::Sender<(String, Result<Value, OsError>)>>);
    impl OsCompletionSink for Collect {
        fn complete(&self, _: &CallSite, key: &str, outcome: Result<Value, OsError>) {
            let _ = self.0.lock().expect("lock").send((key.to_string(), outcome));
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        std::thread::sleep(Duration::from_millis(150));
        stream.write_all(b"hi").expect("write");
        stream
    });
    with_host(tcp_grant(port), |host| {
        let (tx, rx) = mpsc::channel();
        host.handles.completions.set_sink(Arc::new(Collect(Mutex::new(tx))));
        let sock = field(&ask(host, OsOp::TcpConnect, &[s("127.0.0.1"), n(i64::from(port))]).expect("connected"), "sock");
        let site = CallSite {
            orbital: None,
            trait_name: "T".to_string(),
            follow_on: false,
            entity_id: "singleton".to_string(),
            emit: CallRoutes { success: Some("GOT".to_string()), ..CallRoutes::default() },
        };
        let started = Instant::now();
        assert!(recv_later(&[sock.clone(), n(16)], "r1", &site, host).is_ok());
        assert!(started.elapsed() < Duration::from_millis(100), "the call returned before the bytes arrived");
        let (key, outcome) = rx.recv_timeout(Duration::from_secs(5)).expect("the completion arrives");
        assert_eq!(key, "r1");
        assert_eq!(field(&outcome.expect("bytes"), "data"), bytes_value(b"hi"));
        assert!(
            recv_later(&[sock.clone(), n(16)], "r3", &site, host).is_ok()
                && recv_later(&[sock, n(16)], "r3", &site, host).is_err(),
            "control: a key with a completion pending is refused"
        );
        host.handles.completions.cancel("r3");
    });
    drop(server.join());
}

type Fired = (CallSite, String, Result<Value, OsError>);

fn collecting_sink(handles: &OsHandles) -> mpsc::Receiver<Fired> {
    use crate::os_host::completion::OsCompletionSink;
    use std::sync::{Arc, Mutex};
    struct Collect(Mutex<mpsc::Sender<Fired>>);
    impl OsCompletionSink for Collect {
        fn complete(&self, site: &CallSite, key: &str, outcome: Result<Value, OsError>) {
            let _ = self.0.lock().expect("lock").send((site.clone(), key.to_string(), outcome));
        }
    }
    let (tx, rx) = mpsc::channel();
    handles.completions.set_sink(Arc::new(Collect(Mutex::new(tx))));
    rx
}

fn listen_site() -> CallSite {
    CallSite {
        orbital: None,
        trait_name: "T".to_string(),
        follow_on: false,
        entity_id: "singleton".to_string(),
        emit: crate::resumable::CallRoutes {
            success: Some("LISTENING".to_string()),
            failure: Some("FAILED".to_string()),
            on_message: Some("ACCEPTED".to_string()),
            ..crate::resumable::CallRoutes::default()
        },
    }
}

fn id_of(value: &Value) -> i64 {
    match value {
        Value::Number(v) => *v as i64,
        other => panic!("not a handle: {other:?}"),
    }
}

// EXEC-QUEUE-12
#[test]
fn a_keyed_tcp_listen_streams_each_accepted_connection_to_the_message_route_until_closed() {
    use crate::resumable::completion_event;
    let handles = OsHandles::default();
    let rx = collecting_sink(&handles);
    let on_event = |_: Option<&CallSite>, _: Value| {};
    let listen_grants = grants(&[("127.0.0.1:0".to_string(), &[NetKind::Listen])]);
    let listening = OsHost { grants: &listen_grants, handles: &handles, on_event: &on_event, secrets: None };
    let site = listen_site();
    let bound = answer(OsOp::TcpListen, &[s("127.0.0.1"), n(0)], Some("l1"), Some(&site), &listening)
        .expect("net answers tcp-listen")
        .expect("listening");
    let listener = id_of(&field(&bound, "sock"));
    let port = id_of(&field(&bound, "port"));
    assert!(port > 0, "an ephemeral port is reported");
    assert_eq!(field(&bound, "address"), s("127.0.0.1"));
    let Value::Object(fields) = &bound else { panic!("listener reply must be an object") };
    let names: Vec<&str> = fields.keys().map(|k| k.as_str()).collect();
    let contract = orbital_core_base::emit_contract::success_payload("os/tcp-listen", orbital_core_base::emit_contract::CallVariant::Plain);
    assert!(orbital_core_base::emit_contract::undeclared_reads(&names, &contract, &[]).is_empty(), "the published contract must expose every field the listener returns");
    let connect_grants = tcp_grant(port as u16);
    let client = OsHost { grants: &connect_grants, handles: &handles, on_event: &on_event, secrets: None };
    let mut accepted = Vec::new();
    for _ in 0..2 {
        let sock = field(&ask(&client, OsOp::TcpConnect, &[s("127.0.0.1"), n(port)]).expect("connected"), "sock");
        let (at, key, outcome) = rx.recv_timeout(Duration::from_secs(5)).expect("an accepted connection arrives");
        assert_eq!(key, "l1");
        let (event, payload) = completion_event(&at, &key, outcome).expect("routed");
        assert_eq!(event, "ACCEPTED", "each connection fires the message route, never success");
        assert_eq!(payload.get_path("key"), Some(&s("l1")));
        assert!(matches!(payload.get_path("peer"), Some(Value::String(p)) if p.starts_with("127.0.0.1:")));
        let conn = payload.get_path("sock").cloned().expect("the accepted socket");
        assert_ne!(id_of(&conn), listener);
        accepted.push((sock, conn));
    }
    let (client_sock, server_sock) = accepted.pop().expect("one");
    ask(&client, OsOp::Send, &[client_sock, bytes_value(b"ping")]).expect("send");
    let got = ask(&client, OsOp::Recv, &[server_sock, n(16)]).expect("the accepted socket reads");
    assert_eq!(field(&got, "data"), bytes_value(b"ping"));
    assert!(handles.completions.is_live("l1"), "the stream stays open across connections");
    answer(OsOp::Close, &[n(listener)], None, None, &client).expect("a socket").expect("closed");
    let deadline = Instant::now() + Duration::from_secs(5);
    while handles.completions.is_live("l1") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!handles.completions.is_live("l1"), "closing the listener ends the stream and frees its key");
    assert!(
        ask(&client, OsOp::TcpConnect, &[s("127.0.0.1"), n(port)]).is_err(),
        "control: nothing listens after close"
    );
    assert!(rx.recv_timeout(Duration::from_millis(200)).is_err(), "no event after close");
    handles.close_all();
}

fn accept_one(handles: &OsHandles, rx: &mpsc::Receiver<Fired>) -> (i64, TcpStream) {
    let on_event = |_: Option<&CallSite>, _: Value| {};
    let listen_grants = grants(&[("127.0.0.1:0".to_string(), &[NetKind::Listen])]);
    let host = OsHost { grants: &listen_grants, handles, on_event: &on_event, secrets: None };
    let bound = answer(OsOp::TcpListen, &[s("127.0.0.1"), n(0)], Some("lc"), Some(&listen_site()), &host)
        .expect("answered")
        .expect("listening");
    let client = TcpStream::connect(("127.0.0.1", id_of(&field(&bound, "port")) as u16)).expect("connect");
    client.set_read_timeout(Some(Duration::from_secs(2))).expect("timeout");
    let (_, _, outcome) = rx.recv_timeout(Duration::from_secs(5)).expect("accepted");
    (id_of(&field(&outcome.expect("a connection"), "sock")), client)
}

fn outcome_of(rx: &mpsc::Receiver<Fired>, key: &str) -> Result<Value, OsError> {
    loop {
        let (_, got, outcome) = rx.recv_timeout(Duration::from_secs(5)).expect("a completion");
        if got == key {
            return outcome;
        }
    }
}

#[test]
fn an_accepted_connection_takes_sends_over_time_reads_a_second_request_and_reports_a_client_abort() {
    let handles = OsHandles::default();
    let rx = collecting_sink(&handles);
    let (conn, mut client) = accept_one(&handles, &rx);
    let on_event = |_: Option<&CallSite>, _: Value| {};
    let none = OsGrants::default();
    let host = OsHost { grants: &none, handles: &handles, on_event: &on_event, secrets: None };
    client.write_all(b"req1").expect("write");
    assert_eq!(field(&ask(&host, OsOp::Recv, &[n(conn), n(16)]).expect("read"), "data"), bytes_value(b"req1"));
    for chunk in [b"a", b"b"] {
        ask(&host, OsOp::Send, &[n(conn), bytes_value(chunk)]).expect("a chunk is written");
        let mut got = [0u8; 1];
        client.read_exact(&mut got).expect("the chunk arrives before the stream ends");
        assert_eq!(&got, chunk);
        std::thread::sleep(Duration::from_millis(50));
    }
    client.write_all(b"req2").expect("write");
    assert_eq!(
        field(&ask(&host, OsOp::Recv, &[n(conn), n(16)]).expect("read"), "data"),
        bytes_value(b"req2"),
        "the read side stays open after a reply"
    );
    assert!(recv_later(&[n(conn), n(16)], "watch", &listen_site(), &host).is_ok());
    drop(client);
    let ended = outcome_of(&rx, "watch");
    assert!(
        matches!(&ended, Ok(v) if field(v, "eof") == Value::Bool(true))
            || matches!(&ended, Err(e) if e.kind == OsErrorKind::Closed),
        "a client abort reaches the program on the watching read: {ended:?}"
    );
    handles.close_all();
}

fn recv_fired(rx: &mpsc::Receiver<Fired>, within: Duration) -> (String, Result<Value, OsError>) {
    let (_, key, outcome) = rx.recv_timeout(within).expect("a completion");
    (key, outcome)
}

#[test]
fn a_keyed_send_to_a_peer_that_never_reads_returns_at_once_and_completes_later() {
    let handles = OsHandles::default();
    let rx = collecting_sink(&handles);
    let (conn, client) = accept_one(&handles, &rx);
    let on_event = |_: Option<&CallSite>, _: Value| {};
    let none = OsGrants::default();
    let host = OsHost { grants: &none, handles: &handles, on_event: &on_event, secrets: None };
    let big = vec![b'a'; 16 * 1024 * 1024];
    let started = Instant::now();
    assert!(send_later(&[n(conn), bytes_value(&big)], "s1", &listen_site(), &host).is_ok());
    assert!(started.elapsed() < Duration::from_millis(500), "the dispatch is not held by the write: {:?}", started.elapsed());
    // The 5 s bound is per write call: a stalled peer that took part of the payload ends it after a few bounds.
    let (key, outcome) = recv_fired(&rx, Duration::from_secs(30));
    assert_eq!(key, "s1");
    assert!(
        matches!(&outcome, Err(e) if e.kind == OsErrorKind::Timeout)
            || matches!(&outcome, Ok(v) if field(v, "bytes") == Value::Number(big.len() as f64)),
        "the write ends in its own completion: {outcome:?}"
    );
    drop(client);
    handles.close_all();
}

#[test]
fn keyed_sends_on_one_socket_are_written_in_call_order_and_never_interleave() {
    let handles = OsHandles::default();
    let rx = collecting_sink(&handles);
    let (conn, mut client) = accept_one(&handles, &rx);
    let on_event = |_: Option<&CallSite>, _: Value| {};
    let none = OsGrants::default();
    let host = OsHost { grants: &none, handles: &handles, on_event: &on_event, secrets: None };
    let first = vec![b'a'; 2 * 1024 * 1024];
    let total = first.len() + 6;
    assert!(send_later(&[n(conn), bytes_value(&first)], "s1", &listen_site(), &host).is_ok());
    assert!(send_later(&[n(conn), bytes_value(b"bbbb")], "s2", &listen_site(), &host).is_ok());
    let reader = std::thread::spawn(move || {
        let mut got = vec![0u8; total];
        client.read_exact(&mut got).expect("every byte arrives");
        got
    });
    let sent = ask(&host, OsOp::Send, &[n(conn), bytes_value(b"cc")]).expect("control: an unkeyed send answers in place");
    assert_eq!(field(&sent, "bytes"), Value::Number(2.0), "after the keyed sends before it");
    let got = reader.join().expect("reader");
    assert!(got[..first.len()].iter().all(|b| *b == b'a'), "the first send is written whole before the second");
    assert_eq!(&got[first.len()..], b"bbbbcc");
    let order: Vec<String> = (0..2).map(|_| recv_fired(&rx, Duration::from_secs(5)).0).collect();
    assert_eq!(order, vec!["s1", "s2"], "completions follow the write order");
    handles.close_all();
}

#[test]
fn closing_a_connection_with_a_pending_keyed_recv_ends_it_at_once() {
    let handles = OsHandles::default();
    let rx = collecting_sink(&handles);
    let (conn, mut client) = accept_one(&handles, &rx);
    let on_event = |_: Option<&CallSite>, _: Value| {};
    let none = OsGrants::default();
    let host = OsHost { grants: &none, handles: &handles, on_event: &on_event, secrets: None };
    assert!(recv_later(&[n(conn), n(16)], "watch", &listen_site(), &host).is_ok());
    std::thread::sleep(Duration::from_millis(50));
    answer(OsOp::Close, &[n(conn)], None, None, &host).expect("a socket").expect("closed");
    let started = Instant::now();
    let mut buf = [0u8; 4];
    let read = client.read(&mut buf);
    assert!(
        matches!(read, Ok(0)) && started.elapsed() < Duration::from_secs(1),
        "the client sees the end at once, not when the pending read gives up: {read:?} after {:?}",
        started.elapsed()
    );
    let woke = outcome_of(&rx, "watch");
    assert!(woke.is_ok_and(|v| field(&v, "eof") == Value::Bool(true)), "the pending read ends with eof");
    handles.close_all();
}

#[test]
fn cancelling_a_listen_key_ends_its_stream_and_a_listen_needs_a_key_a_sink_and_a_grant() {
    let handles = OsHandles::default();
    let on_event = |_: Option<&CallSite>, _: Value| {};
    let listen_grants = grants(&[("127.0.0.1:0".to_string(), &[NetKind::Listen])]);
    let host = OsHost { grants: &listen_grants, handles: &handles, on_event: &on_event, secrets: None };
    let site = listen_site();
    let args = [s("127.0.0.1"), n(0)];
    let no_sink = answer(OsOp::TcpListen, &args, Some("l0"), Some(&site), &host).expect("answered");
    assert_eq!(kind_of(no_sink), OsErrorKind::Unsupported, "no sink: nowhere to stream to");
    let rx = collecting_sink(&handles);
    let unkeyed = answer(OsOp::TcpListen, &args, None, None, &host).expect("answered");
    assert_eq!(kind_of(unkeyed), OsErrorKind::Invalid, "a stream needs a key");
    with_host(OsGrants::default(), |ungranted| {
        ungranted.handles.completions.set_sink(handles.completions.sink().expect("sink"));
        let denied = answer(OsOp::TcpListen, &args, Some("l9"), Some(&site), ungranted).expect("answered");
        assert_eq!(kind_of(denied), OsErrorKind::Denied);
    });
    let bound = answer(OsOp::TcpListen, &args, Some("l2"), Some(&site), &host)
        .expect("answered")
        .expect("listening");
    let again = answer(OsOp::TcpListen, &args, Some("l2"), Some(&site), &host).expect("answered");
    assert_eq!(kind_of(again), OsErrorKind::Invalid, "control: a live key is refused");
    let port = id_of(&field(&bound, "port"));
    assert!(handles.cancel_completion("l2"));
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut quiet = false;
    while Instant::now() < deadline && !quiet {
        let _ = TcpStream::connect(("127.0.0.1", port as u16));
        quiet = rx.recv_timeout(Duration::from_millis(200)).is_err();
    }
    assert!(quiet, "a cancelled stream delivers nothing");
    handles.close_all();
}

#[test]
fn listening_needs_the_listen_grant_the_program_check_requires_and_a_tcp_grant_alone_is_denied() {
    use orbital_core_base::os_grants::{os_requirements, OsRequirement};
    use orbital_core_base::schema::sexpr::SExpression;
    let program_needs = os_requirements(
        OsOp::TcpListen,
        &[SExpression::string("127.0.0.1"), SExpression::Number(0.0)],
    );
    assert_eq!(
        program_needs,
        Ok(vec![OsRequirement::Net { host: "127.0.0.1".into(), port: 0, kinds: vec![NetKind::Listen] }])
    );
    let site = listen_site();
    let args = [s("127.0.0.1"), n(0)];
    for (op, key) in [(OsOp::TcpListen, "g1")] {
        let handles = OsHandles::default();
        let _rx = collecting_sink(&handles);
        let on_event = |_: Option<&CallSite>, _: Value| {};
        let listen_only = grants(&[("127.0.0.1:0".to_string(), &[NetKind::Listen])]);
        let host = OsHost { grants: &listen_only, handles: &handles, on_event: &on_event, secrets: None };
        assert!(
            answer(op, &args, Some(key), Some(&site), &host).expect("answered").is_ok(),
            "{}: a listen grant alone listens",
            op.name()
        );
        let tcp_only = grants(&[("127.0.0.1:0".to_string(), &[NetKind::Tcp])]);
        let host = OsHost { grants: &tcp_only, handles: &handles, on_event: &on_event, secrets: None };
        let denied = answer(op, &args, Some("other"), Some(&site), &host).expect("answered");
        assert_eq!(kind_of(denied), OsErrorKind::Denied, "control {}: a tcp grant does not listen", op.name());
        handles.close_all();
    }
}

#[test]
fn tcp_connect_send_recv_round_trip_then_eof() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let server = std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().expect("accept");
        let mut buf = [0u8; 4];
        conn.read_exact(&mut buf).expect("read");
        assert_eq!(&buf, b"ping");
        conn.write_all(b"pong").expect("write");
    });
    with_host(tcp_grant(port), |host| {
        let opened =
            ask(host, OsOp::TcpConnect, &[s("127.0.0.1"), n(port as i64)]).expect("connect");
        let sock = field(&opened, "sock");
        let sent = ask(host, OsOp::Send, &[sock.clone(), bytes_value(b"ping")]).expect("send");
        assert_eq!(field(&sent, "bytes"), Value::Number(4.0));
        let got = ask(host, OsOp::Recv, &[sock.clone(), n(16)]).expect("recv");
        assert_eq!(field(&got, "data"), bytes_value(b"pong"));
        assert_eq!(field(&got, "eof"), Value::Bool(false));
        server.join().expect("server");
        let end = ask(host, OsOp::Recv, &[sock.clone(), n(16)]).expect("eof");
        assert_eq!(field(&end, "data"), s(""));
        assert_eq!(field(&end, "eof"), Value::Bool(true));
        let zero = ask(host, OsOp::Recv, &[sock, n(0)]).expect("zero");
        assert_eq!(field(&zero, "eof"), Value::Bool(false));
    });
}

#[test]
fn close_removes_the_socket_and_later_use_is_closed() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    with_host(tcp_grant(port), |host| {
        let opened =
            ask(host, OsOp::TcpConnect, &[s("127.0.0.1"), n(port as i64)]).expect("connect");
        let sock = field(&opened, "sock");
        let closed = answer(OsOp::Close, std::slice::from_ref(&sock), None, None, host)
            .expect("a socket")
            .expect("closed");
        assert_eq!(closed, Value::object(ValueMap::default()));
        assert_eq!(
            kind_of(ask(host, OsOp::Send, &[sock.clone(), s("aGk=")])),
            OsErrorKind::Closed
        );
        assert_eq!(
            kind_of(ask(host, OsOp::Recv, &[sock.clone(), n(1)])),
            OsErrorKind::Closed
        );
        assert!(answer(OsOp::Close, &[sock], None, None, host).is_none());
        assert!(answer(OsOp::Close, &[n(9999)], None, None, host).is_none());
    });
}

#[test]
fn connect_is_denied_without_a_matching_grant() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let args = [s("127.0.0.1"), n(port as i64)];
    with_host(OsGrants::default(), |host| {
        assert_eq!(
            kind_of(ask(host, OsOp::TcpConnect, &args)),
            OsErrorKind::Denied
        );
    });
    with_host(tcp_grant(port.wrapping_add(1)), |host| {
        assert_eq!(
            kind_of(ask(host, OsOp::TcpConnect, &args)),
            OsErrorKind::Denied
        );
    });
    with_host(
        grants(&[(format!("127.0.0.1:{port}"), &[NetKind::Udp])]),
        |host| {
            assert_eq!(
                kind_of(ask(host, OsOp::TcpConnect, &args)),
                OsErrorKind::Denied
            );
        },
    );
}

#[test]
fn a_granted_port_on_another_host_is_denied() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    with_host(tcp_grant(port), |host| {
        assert_eq!(
            kind_of(ask(
                host,
                OsOp::TcpConnect,
                &[s("127.0.0.2"), n(port as i64)]
            )),
            OsErrorKind::Denied
        );
    });
}

#[test]
fn a_host_name_resolving_to_loopback_is_refused_unless_the_grant_names_the_ip() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    with_host(
        grants(&[(format!("localhost:{port}"), &[NetKind::Tcp])]),
        |host| {
            let refused = ask(host, OsOp::TcpConnect, &[s("localhost"), n(port as i64)])
                .expect_err("refused");
            assert_eq!(refused.kind, OsErrorKind::Denied);
            assert!(
                refused.message.contains("not a public address"),
                "{}",
                refused.message
            );
        },
    );
    with_host(tcp_grant(port), |host| {
        assert!(ask(host, OsOp::TcpConnect, &[s("127.0.0.1"), n(port as i64)]).is_ok());
    });
}

#[test]
fn udp_bind_sendto_recvfrom_round_trip() {
    let peer = UdpSocket::bind("127.0.0.1:0").expect("peer");
    peer.set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    let peer_addr = peer.local_addr().expect("addr");
    let free = UdpSocket::bind("127.0.0.1:0").expect("free");
    let port = free.local_addr().expect("addr").port();
    drop(free);
    let g = grants(&[
        (format!("127.0.0.1:{port}"), &[NetKind::Udp]),
        (format!("127.0.0.1:{}", peer_addr.port()), &[NetKind::Udp]),
    ]);
    with_host(g, |host| {
        let bound = ask(host, OsOp::UdpBind, &[s("127.0.0.1"), n(port as i64)]).expect("bind");
        let sock = field(&bound, "sock");
        let sent = ask(
            host,
            OsOp::Sendto,
            &[
                sock.clone(),
                bytes_value(b"hi"),
                s("127.0.0.1"),
                n(peer_addr.port() as i64),
            ],
        )
        .expect("sendto");
        assert_eq!(field(&sent, "bytes"), Value::Number(2.0));
        let mut buf = [0u8; 16];
        let (len, from) = peer.recv_from(&mut buf).expect("peer recv");
        assert_eq!(&buf[..len], b"hi");
        assert_eq!(from.port(), port);
        peer.send_to(b"yo", from).expect("reply");
        let got = ask(host, OsOp::Recvfrom, &[sock.clone(), n(64)]).expect("recvfrom");
        assert_eq!(field(&got, "data"), bytes_value(b"yo"));
        assert_eq!(field(&got, "peer"), Value::string(peer_addr.to_string()));
        let other = ask(
            host,
            OsOp::Sendto,
            &[sock.clone(), s("aGk="), s("127.0.0.1"), n(1)],
        );
        assert_eq!(kind_of(other), OsErrorKind::Denied);
        assert_eq!(
            kind_of(ask(host, OsOp::Recv, &[sock, n(1)])),
            OsErrorKind::Invalid
        );
    });
    with_host(OsGrants::default(), |host| {
        assert_eq!(
            kind_of(ask(host, OsOp::UdpBind, &[s("127.0.0.1"), n(port as i64)])),
            OsErrorKind::Denied
        );
    });
}

#[test]
fn dns_resolve_needs_a_name_grant_and_filters_by_family() {
    let args = [s("localhost")];
    with_host(OsGrants::default(), |host| {
        assert_eq!(
            kind_of(ask(host, OsOp::DnsResolve, &args)),
            OsErrorKind::Denied
        );
    });
    with_host(
        grants(&[("localhost:0".to_string(), &[NetKind::Tcp])]),
        |host| {
            let all = ask(host, OsOp::DnsResolve, &args).expect("resolve");
            let Value::Array(list) = field(&all, "addresses") else {
                panic!("array")
            };
            assert!(!list.is_empty());
            assert!(list.iter().all(|v| matches!(v, Value::String(_))));
            let four = ask(host, OsOp::DnsResolve, &[s("localhost"), n(4)]).expect("v4");
            let Value::Array(list) = field(&four, "addresses") else {
                panic!("array")
            };
            assert!(list
                .iter()
                .all(|v| matches!(v, Value::String(t) if !t.contains(':'))));
            assert_eq!(
                kind_of(ask(host, OsOp::DnsResolve, &[s("localhost"), n(5)])),
                OsErrorKind::Invalid
            );
            assert_eq!(
                kind_of(ask(host, OsOp::DnsResolve, &[s("other.example")])),
                OsErrorKind::Denied
            );
        },
    );
    with_host(
        grants(&[("127.0.0.1:0".to_string(), &[NetKind::Udp])]),
        |host| {
            let lit = ask(host, OsOp::DnsResolve, &[s("127.0.0.1")]).expect("literal");
            assert_eq!(field(&lit, "addresses"), Value::array(vec![s("127.0.0.1")]));
        },
    );
}

#[test]
fn recv_times_out_instead_of_hanging() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let (release, held) = mpsc::channel::<()>();
    let server = std::thread::spawn(move || {
        let (_conn, _) = listener.accept().expect("accept");
        let _ = held.recv();
    });
    with_host(tcp_grant(port), |host| {
        let opened =
            ask(host, OsOp::TcpConnect, &[s("127.0.0.1"), n(port as i64)]).expect("connect");
        let sock = field(&opened, "sock");
        let id = match sock {
            Value::Number(v) => v as i64,
            _ => panic!("sock"),
        };
        host.handles.net.socks.with(id, |entry| {
            if let SockEntry::Stream(stream) = entry {
                stream
                    .set_read_timeout(Some(Duration::from_millis(100)))
                    .expect("timeout");
            }
        });
        let started = Instant::now();
        let result = ask(host, OsOp::Recv, &[n(id), n(8)]);
        assert_eq!(kind_of(result), OsErrorKind::Timeout);
        assert!(started.elapsed() < Duration::from_secs(3));
    });
    release.send(()).expect("release");
    server.join().expect("server");
}

#[test]
fn shutdown_ends_the_stream_and_a_datagram_socket_is_not_a_stream() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    with_host(tcp_grant(port), |host| {
        let opened =
            ask(host, OsOp::TcpConnect, &[s("127.0.0.1"), n(port as i64)]).expect("connect");
        let sock = field(&opened, "sock");
        assert_eq!(
            ask(host, OsOp::Shutdown, std::slice::from_ref(&sock)).expect("shutdown"),
            Value::object(ValueMap::default())
        );
        let after = kind_of(ask(host, OsOp::Send, &[sock, s("aGk=")]));
        assert!(
            matches!(after, OsErrorKind::Closed | OsErrorKind::Io),
            "{after:?}"
        );
        assert_eq!(
            kind_of(ask(host, OsOp::Shutdown, &[n(4242)])),
            OsErrorKind::Closed
        );
    });
}

#[test]
fn watching_a_port_is_unsupported_and_bad_arguments_are_invalid() {
    with_host(OsGrants::default(), |host| {
        let error = ask(host, OsOp::WatchPort, &[n(80), s("tcp")]).expect_err("unsupported");
        assert_eq!(error.kind, OsErrorKind::Unsupported);
        assert!(error.message.contains("streaming host not implemented"));
        for args in [
            vec![s("127.0.0.1"), n(70_000)],
            vec![s("127.0.0.1"), n(-1)],
            vec![s(""), n(80)],
            vec![n(1), n(80)],
        ] {
            assert_eq!(
                kind_of(ask(host, OsOp::TcpConnect, &args)),
                OsErrorKind::Invalid
            );
        }
        assert_eq!(
            kind_of(ask(host, OsOp::Recv, &[n(3), n(-1)])),
            OsErrorKind::Invalid
        );
        assert_eq!(
            kind_of(ask(host, OsOp::Send, &[n(3), s("!!")])),
            OsErrorKind::Invalid
        );
        assert!(answer(OsOp::Cwd, &[], None, None, host).is_none());
    });
}

use crate::os_host::answer_os;
use orbital_core_base::secret::Secret;
use crate::resumable::{IoAnswer, IoRequest};

struct Held(&'static str, &'static str);

impl crate::secret_source::SecretSource for Held {
    fn secret(&self, name: &str) -> Result<Option<String>, String> {
        Ok((name == self.0).then(|| self.1.to_string()))
    }
}

fn send_request(sock: &Value, segments: Vec<Value>) -> IoRequest {
    IoRequest::Os { op: OsOp::Send, args: vec![sock.clone(), Value::array(segments)], key: None, site: None }
}

fn answered(host: &OsHost<'_>, request: &IoRequest) -> Result<Value, OsError> {
    match answer_os(Some(host), request) {
        IoAnswer::Os(answer) => answer.map(Secret::into_inner),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_os_send_credential_segment_is_filled_in_at_the_boundary_and_the_request_names_only_the_credential() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let server = std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().expect("accept");
        let mut buf = vec![0u8; 23];
        conn.read_exact(&mut buf).expect("read");
        buf
    });
    let (grants, handles) = (tcp_grant(port), OsHandles::default());
    let on_event = |_: Option<&CallSite>, _: Value| {};
    let held = Held("REDIS_PW", "s3cret-pässword");
    let host = OsHost { grants: &grants, handles: &handles, on_event: &on_event, secrets: Some(&held) };
    let sock = field(&ask(&host, OsOp::TcpConnect, &[s("127.0.0.1"), n(port as i64)]).expect("connect"), "sock");
    let segment = |text: &str, credential: &str| {
        Value::object(orbital_core_base::value::ValueMap::from_iter([("bytes".into(), bytes_value(text.as_bytes())), ("credential".into(), s(credential))]))
    };
    let request = send_request(&sock, vec![segment("AUTH ", "REDIS_PW"), segment("\r\n", "")]);
    let shown = format!("{request:?}");
    assert!(shown.contains("REDIS_PW") && !shown.contains("s3cret"), "the journaled request names the credential only: {shown}");
    let sent = answered(&host, &request).expect("send");
    assert_eq!(field(&sent, "bytes"), Value::Number(23.0), "the credential's UTF-8 bytes are counted");
    assert_eq!(server.join().expect("server"), "AUTH s3cret-pässword\r\n".as_bytes());
    assert!(!format!("{sent:?}").contains("s3cret"), "the answer holds no value");

    let unheld = send_request(&sock, vec![segment("x", "OTHER")]);
    assert_eq!(kind_of(answered(&host, &unheld)), OsErrorKind::Denied, "an unheld credential sends nothing");
    let malformed = send_request(&sock, vec![Value::object(orbital_core_base::value::ValueMap::from_iter([("bytes".into(), s("%%")), ("credential".into(), s(""))]))]);
    assert_eq!(kind_of(answered(&host, &malformed)), OsErrorKind::Invalid);
    let nameless = send_request(&sock, vec![Value::object(orbital_core_base::value::ValueMap::from_iter([("bytes".into(), s(""))]))]);
    assert_eq!(kind_of(answered(&host, &nameless)), OsErrorKind::Invalid);
    let bare = send_request(&sock, vec![s("AAAA")]);
    assert_eq!(kind_of(answered(&host, &bare)), OsErrorKind::Invalid, "a segment is an object");
    let blind = OsHost { grants: &grants, handles: &handles, on_event: &on_event, secrets: None };
    assert_eq!(kind_of(answered(&blind, &send_request(&sock, vec![segment("", "REDIS_PW")]))), OsErrorKind::Denied, "no credential source");
    assert!(answered(&blind, &send_request(&sock, vec![segment("", "")])).is_ok(), "control: bytes alone need no source");
    handles.close_all();
}
