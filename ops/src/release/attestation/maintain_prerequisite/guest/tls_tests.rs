use super::super::materials::tests::{Corpus, corpus, wrong_name_corpus};
use super::*;
use openssl::ssl::SslAcceptor;
use std::{net::TcpListener, thread};

/// Both real endpoints have finite I/O ceilings and the server is always joined.
fn server_case(include_root: bool, omit_intermediate: bool, status: u16) -> Result<TlsProof> {
    server_fixture(
        corpus(),
        include_root,
        omit_intermediate,
        status,
        None,
        true,
    )
}
fn server_fixture(
    fixture: Corpus,
    include_root: bool,
    omit_intermediate: bool,
    status: u16,
    trust: Option<X509>,
    bind: bool,
) -> Result<TlsProof> {
    let materials = if bind {
        Some(materials::analyze(&fixture.materials, fixture.now, Duration::from_secs(600)).unwrap())
    } else {
        None
    };
    let mut builder = SslAcceptor::mozilla_intermediate(SslMethod::tls_server()).unwrap();
    builder.set_certificate(&fixture.leaf).unwrap();
    builder.set_private_key(&fixture.leaf_key).unwrap();
    if !omit_intermediate {
        builder
            .add_extra_chain_cert(fixture.intermediate.clone())
            .unwrap();
    }
    if include_root {
        builder.add_extra_chain_cert(fixture.root.clone()).unwrap();
    }
    let acceptor = builder.build();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let std::net::SocketAddr::V4(address) = listener.local_addr().unwrap() else {
        panic!("IPv4 listener")
    };
    let mut control = Control::new(Instant::now()).unwrap();
    let end = attempt_end(&mut control).unwrap();
    let result = thread::scope(|scope| {
        let server=scope.spawn(move || {
            let end=Instant::now()+Duration::from_secs(4);
            let stream=loop {
                match listener.accept() {
                    Ok((stream,_))=>break stream,
                    Err(error) if error.kind()==io::ErrorKind::WouldBlock && Instant::now()<end=>thread::sleep(Duration::from_millis(5)),
                    Err(_)=>return false,
                }
            };
            // Darwin can inherit O_NONBLOCK from the listener. This synthetic
            // peer uses blocking SSL with finite I/O ceilings on both platforms.
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            stream.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
            let Ok(mut stream)=acceptor.accept(stream) else {return omit_intermediate;};
            let mut request=Vec::new();let mut block=[0u8;512];
            while !request.ends_with(b"\r\n\r\n") {
                let Ok(count)=stream.read(&mut block) else {return false;};
                if count==0 || request.len()+count>4096 {return false;}
                request.extend_from_slice(&block[..count]);
            }
            if request!=REQUEST {return false;}
            stream.write_all(format!("HTTP/1.1 {status} fixture\r\nContent-Length: 3\r\nConnection: close\r\n\r\n404").as_bytes()).is_ok()
        });
        let result = exchange(
            address,
            trust.unwrap_or_else(|| fixture.root.clone()),
            &mut control,
            end,
        );
        let served = server.join().unwrap();
        assert!(served || result.is_err());
        result
    });
    if let Ok(proof) = &result {
        let materials = materials.as_ref().unwrap();
        proof.bind(materials).unwrap();
        assert_eq!(proof.verified_hashes, materials.der_hashes);
        assert_eq!(proof.served_hashes.len(), if include_root { 3 } else { 2 });
    }
    result
}
#[test]
fn actual_tls_reads_distinct_verified_and_served_chains_and_exact_metrics_request() {
    assert!(server_case(false, false, 404).is_ok());
    assert!(server_case(true, false, 404).is_ok());
}
#[test]
fn actual_missing_intermediate_and_wrong_http_status_do_not_complete() {
    assert!(server_case(false, true, 404).is_err());
    assert!(matches!(
        server_case(false, false, 200),
        Err(Error::Refused)
    ));
}
#[test]
fn actual_hostname_and_unrelated_held_root_refuse_the_handshake() {
    assert!(server_fixture(wrong_name_corpus(), false, false, 404, None, false).is_err());
    let other = corpus();
    assert!(server_fixture(corpus(), false, false, 404, Some(other.root), false).is_err());
}
#[test]
fn actual_chain_proof_binds_complete_ders_not_only_a_successful_connection() {
    let proof = server_case(false, false, 404).unwrap();
    let unrelated = corpus();
    let unrelated = materials::analyze(
        &unrelated.materials,
        unrelated.now,
        Duration::from_secs(600),
    )
    .unwrap();
    assert!(proof.bind(&unrelated).is_err());
}
#[test]
fn bounded_http_framing_covers_fixed_chunked_close_and_invalid_neighbors() {
    let fixed = b"HTTP/1.1 404 Not Found\r\nContent-Length: 3\r\n\r\n404";
    let (offset, mode) = headers(fixed).unwrap().unwrap();
    assert!(body_done(&fixed[offset..], mode, false).unwrap());
    let chunked =
        b"HTTP/1.1 404 Not Found\r\nTransfer-Encoding: chunked\r\n\r\n3\r\n404\r\n0\r\n\r\n";
    let (offset, mode) = headers(chunked).unwrap().unwrap();
    assert!(body_done(&chunked[offset..], mode, false).unwrap());
    assert!(!body_done(b"3\r\n40", Body::Chunked, false).unwrap());
    assert!(body_done(b"3\r\n40", Body::Chunked, true).is_err());
    assert!(body_done(b"404", Body::Close, true).unwrap());
    for response in [
        b"HTTP/1.1 200 OK\r\n\r\n".as_slice(),
        b"HTTP/1.1 404 Nope\r\nContent-Length: 65537\r\n\r\n",
        b"HTTP/1.1 404 Nope\r\nContent-Length: 3\r\nTransfer-Encoding: chunked\r\n\r\n",
        b"HTTP/1.1 404 Nope\r\nContent-Length: -1\r\n\r\n",
    ] {
        assert!(headers(response).is_err());
    }
    assert!(body_done(&vec![b'x'; MAX_BODY + 1], Body::Close, true).is_err());
    assert!(body_done(b"10001\r\n", Body::Chunked, false).is_err());
}
#[test]
fn original_http_line_caps_include_crlf_and_header_terminator_count() {
    let header = |size: usize| {
        let mut bytes = b"HTTP/1.1 404 Not Found\r\nX-Large: ".to_vec();
        bytes.extend(vec![b'a'; size]);
        bytes.extend_from_slice(b"\r\nContent-Length: 0\r\n\r\n");
        bytes
    };
    assert!(headers(&header(MAX_LINE - 11)).unwrap().is_some());
    assert!(matches!(
        headers(&header(MAX_LINE - 10)),
        Err(Error::Bounds)
    ));
    let mut fields = b"HTTP/1.1 404 Not Found\r\n".to_vec();
    for _ in 0..99 {
        fields.extend_from_slice(b"X: a\r\n");
    }
    let mut valid = fields.clone();
    valid.extend_from_slice(b"\r\n");
    assert!(headers(&valid).unwrap().is_some());
    fields.extend_from_slice(b"X: a\r\n\r\n");
    assert!(headers(&fields).is_err());
    let chunk = |size: usize| {
        let mut bytes = b"0;".to_vec();
        bytes.extend(vec![b'a'; size]);
        bytes.extend_from_slice(b"\r\n\r\n");
        bytes
    };
    assert!(body_done(&chunk(MAX_LINE - 4), Body::Chunked, false).unwrap());
    assert!(matches!(
        body_done(&chunk(MAX_LINE - 3), Body::Chunked, false),
        Err(Error::Bounds)
    ));
    let mut trailer = b"0\r\nX-Large: ".to_vec();
    trailer.extend(vec![b'a'; MAX_LINE]);
    trailer.extend_from_slice(b"\r\n\r\n");
    assert!(matches!(
        body_done(&trailer, Body::Chunked, false),
        Err(Error::Bounds)
    ));
}
#[test]
fn explicit_original_close_is_idempotent_and_late_failure_preserves_primary() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (peer, _) = listener.accept().unwrap();
    let socket = Socket(Arc::new(Mutex::new(Some(stream))));
    let mut borrower = socket.clone();
    assert!(socket.close().is_ok());
    assert!(socket.close().is_ok());
    assert!(borrower.write(b"closed").is_err());
    nix::unistd::close(peer).unwrap();
    let mut control = Control::new(Instant::now()).unwrap();
    let result = settled(
        Err(Error::Io),
        Err(Error::Close),
        &mut control,
        Instant::now() - Duration::from_millis(1),
    );
    assert!(matches!(result, Err(Error::Io)));
    assert_eq!(control.first, Some(Error::Io));
    assert!(control.cleanup_failed);
    let mut control = Control::new(Instant::now()).unwrap();
    let result = settled(
        Err(Error::Io),
        Ok(()),
        &mut control,
        Instant::now() - Duration::from_millis(1),
    );
    assert!(matches!(result, Err(Error::Io)));
    assert_eq!(control.first, Some(Error::Io));
    assert!(!control.cleanup_failed);
    control.check().unwrap(); // Outer body is live; the failed attempt cannot retry.
    let mut control = Control::new(Instant::now() - Duration::from_secs(31)).unwrap();
    let original_end = control.end();
    let result = settled(
        Err(Error::Io),
        Ok(()),
        &mut control,
        Instant::now() + Duration::from_secs(1),
    );
    assert!(matches!(result, Err(Error::Io)));
    assert_eq!(control.first, Some(Error::Io));
    assert_eq!(control.status, 124);
    assert_eq!(control.end(), original_end);
    assert!(!control.cleanup_failed);
    let mut control = Control::new(Instant::now()).unwrap();
    let result = settled(
        Err(Error::Io),
        Ok(()),
        &mut control,
        Instant::now() + Duration::from_secs(1),
    );
    assert!(matches!(result, Err(Error::Io)));
    assert_eq!(control.first, None); // An on-time closed transport refusal can retry.
    assert!(!control.cleanup_failed);
    let mut control = Control::new(Instant::now() - Duration::from_secs(31)).unwrap();
    assert!(matches!(
        probe_root(Ipv4Addr::LOCALHOST, b"not a certificate", &mut control),
        Err(Error::Deadline)
    ));
}
