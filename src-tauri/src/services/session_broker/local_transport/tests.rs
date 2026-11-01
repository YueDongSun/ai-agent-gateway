use super::*;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(3)
}
fn limits() -> TransportLimits {
    TransportLimits {
        frame_bytes: 4096,
        queued_bytes: 8192,
        queued_frames: 4,
        connections: 2,
    }
}
fn frame(raw: &str) -> TransportFrame {
    TransportFrame::parse(raw.as_bytes().to_vec(), 4096).unwrap()
}

#[test]
fn pipe_names_are_derived_from_fixed_service_roles_and_validated_generations() {
    assert_eq!(
        PipeName::for_role(ServiceRole::Policy).unwrap().as_str(),
        r"\\.\pipe\aag_session_authority_rpc_v1"
    );
    // Synthetic Windows named-pipe fixture: this is the fixed IPC namespace.
    assert_eq!(
        PipeName::for_role(ServiceRole::Reader).unwrap().as_str(),
        r"\\.\pipe\aag_session_reader_rpc_v1"
    );
    let role = ServiceRole::runtime("0123456789abcdef0123456789abcdef", 7).unwrap();
    assert_eq!(
        PipeName::for_role(role).unwrap().as_str(),
        r"\\.\pipe\aag_session_runtime_0123456789abcdef0123456789abcdef_g7_rpc_v1"
    );
}

#[test]
fn production_peer_requires_a_complete_virtual_service_sid() {
    assert!(ServicePeer::new(
        ServiceRole::Policy,
        Sid::parse("S-1-5-80-1-2-3-4-5").unwrap()
    )
    .is_ok());
    for sid in ["S-1-5-21-1-2-3-1001", "S-1-5-80-1-2-3-4", "S-1-1-0"] {
        assert!(matches!(
            ServicePeer::new(ServiceRole::Policy, Sid::parse(sid).unwrap()),
            Err(TransportError::InvalidEndpoint)
        ));
    }
}

#[test]
fn codec_uses_fixed_network_length_and_preserves_opaque_bytes() {
    let value = frame(r#"{"method":"x"}"#);
    assert_eq!(codec::encode_length(&value), [0, 0, 0, 14]);
    let decoded = codec::decode_wire(b"\0\0\0\x0e{\"method\":\"x\"}", 4096).unwrap();
    assert_eq!(decoded.bytes(), value.bytes());
    let raw = b"{\n\"id\":7,\"result\":{\"encrypted_content\":\"opaque\\u0000\",\"future\":123456789012345678901234567890}}";
    let parsed = TransportFrame::parse(raw.to_vec(), 4096).unwrap();
    assert_eq!(parsed.bytes(), raw);
    assert_eq!(parsed.kind(), EnvelopeKind::Response);
}

#[test]
fn codec_rejects_oversize_truncation_invalid_utf8_and_ambiguous_routing() {
    assert!(matches!(
        codec::decode_length([0, 0, 1, 1], 256),
        Err(TransportError::FrameTooLarge)
    ));
    assert!(matches!(
        codec::decode_wire(&[0, 0, 0], 4096),
        Err(TransportError::TruncatedFrame)
    ));
    assert!(matches!(
        codec::decode_wire(&[0, 0, 0, 14, b'{'], 4096),
        Err(TransportError::TruncatedFrame)
    ));
    assert!(matches!(
        TransportFrame::parse(vec![0xff], 4096),
        Err(TransportError::InvalidUtf8)
    ));
    for raw in [
        r#"{"id":1,"id":2,"method":"x"}"#,
        r#"{"method":"x","method":"y"}"#,
        r#"{"id":1,"result":{},"error":{}}"#,
        r#"{"id":1,"method":"x","result":{}}"#,
        r#"{"method":"x","params":{},"params":{}}"#,
    ] {
        assert!(
            matches!(
                TransportFrame::parse(raw.as_bytes().to_vec(), 4096),
                Err(TransportError::InvalidFrame)
            ),
            "{raw}"
        );
    }
}

#[test]
fn unknown_methods_and_fields_are_not_executed_or_discarded_by_transport() {
    let raw = r#"{"id":"future_id","method":"future/unknown","params":{"opaque":{"new_field":true}},"future_header":17}"#;
    let parsed = frame(raw);
    assert_eq!(parsed.kind(), EnvelopeKind::Request);
    assert_eq!(parsed.bytes(), raw.as_bytes());
}

#[tokio::test]
async fn owned_local_pipe_round_trip_attests_owner_and_preserves_frames() {
    let endpoint = pipe::FixtureEndpoint::new().unwrap();
    let listener = pipe::fixture_listener(&endpoint, limits()).unwrap();
    let (client, server) = tokio::join!(
        pipe::fixture_connect(&endpoint, endpoint.owner(), limits(), deadline()),
        listener.accept(deadline()),
    );
    let client = client.unwrap();
    let server = server.unwrap();
    assert!(client.peer().owner_sid() == endpoint.owner());
    let raw = r#"{"id":7,"method":"future/opaque","params":{"encrypted_content":"preserved","future":123456789012345678901234567890}}"#;
    let (sent, received) = tokio::join!(
        client.send(frame(raw), deadline()),
        server.receive(deadline())
    );
    assert_eq!(sent, SendOutcome::Written);
    assert_eq!(received.unwrap().bytes(), raw.as_bytes());
    assert_eq!(client.close(deadline()).await, TransportClosed::Closed);
    assert_eq!(server.close(deadline()).await, TransportClosed::Closed);
    assert_eq!(listener.close(deadline()).await, TransportClosed::Closed);
}

#[tokio::test]
async fn connected_pipe_owner_is_checked_before_admitting_a_client() {
    let endpoint = pipe::FixtureEndpoint::new().unwrap();
    let listener = pipe::fixture_listener(&endpoint, limits()).unwrap();
    let wrong = Sid::parse("S-1-5-80-1-2-3-4-5").unwrap();
    let result = pipe::fixture_connect(&endpoint, &wrong, limits(), deadline()).await;
    assert!(matches!(result, Err(TransportError::PeerOwnerMismatch)));
    assert_eq!(listener.close(deadline()).await, TransportClosed::Closed);
}

#[tokio::test(flavor = "current_thread")]
async fn early_disconnected_client_does_not_close_the_listener() {
    let endpoint = pipe::FixtureEndpoint::new().unwrap();
    let listener = pipe::fixture_listener(&endpoint, limits()).unwrap();
    // No await before the close: the accept task has not been polled yet.
    fixture::fixture_disconnect_before_accept(&endpoint).unwrap();
    let (client, server) = tokio::join!(
        pipe::fixture_connect(&endpoint, endpoint.owner(), limits(), deadline()),
        listener.accept(deadline()),
    );
    let first_connection =
        server.expect("the first disconnected client is still an accepted connection");
    let client = client.expect("a subsequent owned client must connect");
    // Mio admits the earlier connection and reports its closure through EOF.
    // Consume that distinct connection before accepting the live second one.
    assert!(matches!(
        first_connection.receive(deadline()).await,
        Err(TransportError::Closed)
    ));
    assert!(!listener.cancellation().is_cancelled());
    assert_eq!(
        first_connection.close(deadline()).await,
        TransportClosed::Closed
    );
    let server = listener
        .accept(deadline())
        .await
        .expect("listener must accept the second live client");
    let raw = r#"{"id":17,"method":"fixture/after_disconnect"}"#;
    let (sent, received) = tokio::join!(
        client.send(frame(raw), deadline()),
        server.receive(deadline()),
    );
    assert_eq!(sent, SendOutcome::Written);
    assert_eq!(received.unwrap().bytes(), raw.as_bytes());
    assert_eq!(client.close(deadline()).await, TransportClosed::Closed);
    assert_eq!(server.close(deadline()).await, TransportClosed::Closed);
    assert_eq!(listener.close(deadline()).await, TransportClosed::Closed);
}

#[tokio::test]
async fn a_second_first_instance_fails_closed() {
    let endpoint = pipe::FixtureEndpoint::new().unwrap();
    let first = pipe::fixture_listener(&endpoint, limits()).unwrap();
    assert!(matches!(
        pipe::fixture_listener(&endpoint, limits()),
        Err(TransportError::NamespaceOccupied)
    ));
    assert_eq!(first.close(deadline()).await, TransportClosed::Closed);
}

#[tokio::test]
async fn client_data_access_cannot_create_an_additional_server_instance() {
    let evidence = pipe::fixture_access_probe(limits()).await.unwrap();
    assert!(evidence.opened_rw);
    assert!(evidence.generic_write_denied);
    assert!(evidence.instance_creation_denied);
    assert_eq!(evidence.granted_access & 0x4, 0);
    assert_eq!(evidence.granted_access & 0x0012_0183, 0x0012_0183);
}

#[tokio::test]
async fn listener_cancellation_closes_its_owned_namespace() {
    let endpoint = pipe::FixtureEndpoint::new().unwrap();
    let listener = pipe::fixture_listener(&endpoint, limits()).unwrap();
    listener.cancellation().cancel();
    assert!(matches!(
        listener.accept(deadline()).await,
        Err(TransportError::Cancelled)
    ));
    assert_eq!(listener.close(deadline()).await, TransportClosed::Closed);
    assert!(matches!(
        pipe::fixture_connect(&endpoint, endpoint.owner(), limits(), deadline()).await,
        Err(TransportError::EndpointUnavailable)
    ));
}

#[tokio::test]
async fn partial_frame_eof_is_reported_without_a_payload() {
    let (stream, mut peer) = tokio::io::duplex(32);
    let connection = pipe::fixture_duplex(stream, limits()).unwrap();
    peer.write_all(&[0, 0, 0, 14, b'{']).await.unwrap();
    drop(peer);
    assert!(matches!(
        connection.receive(deadline()).await,
        Err(TransportError::TruncatedFrame)
    ));
    assert_eq!(connection.close(deadline()).await, TransportClosed::Closed);
}

#[tokio::test]
async fn a_queued_send_timeout_is_unknown_and_closes_without_retry() {
    let (stream, _unread_peer) = tokio::io::duplex(8);
    let connection = pipe::fixture_duplex(stream, limits()).unwrap();
    let result = connection.send(frame(r#"{"id":1,"method":"opaque/request","params":{"value":"never replay this frame"}}"#), Instant::now() + Duration::from_millis(50)).await;
    assert!(matches!(
        result,
        SendOutcome::OutcomeUnknown(TransportError::DeadlineExceeded)
    ));
    assert_eq!(connection.close(deadline()).await, TransportClosed::Closed);
    assert!(matches!(
        connection
            .send(frame(r#"{"method":"x"}"#), deadline())
            .await,
        SendOutcome::NotSent(_)
    ));
}

#[tokio::test]
async fn cancellation_before_queueing_is_definitely_not_sent() {
    let (stream, _peer) = tokio::io::duplex(8);
    let connection = pipe::fixture_duplex(stream, limits()).unwrap();
    connection.cancellation().cancel();
    assert!(matches!(
        connection
            .send(frame(r#"{"method":"x"}"#), deadline())
            .await,
        SendOutcome::NotSent(TransportError::Cancelled)
    ));
    assert_eq!(connection.close(deadline()).await, TransportClosed::Closed);
}
