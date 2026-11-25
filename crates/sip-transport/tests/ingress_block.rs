// siphon-rs - The Siphon SIP Stack
// Copyright (C) 2026 siphon-rs contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//
//! `set_ingress_block` against the real receive loops: a blocked source's
//! UDP packets never reach the channel and its TCP connections are closed
//! as they are accepted; lifting the block lets it through again.
//!
//! The blocked source is 127.0.0.2, so nothing else on loopback is touched.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use siphon_transport::{run_tcp, run_udp, set_ingress_block, InboundPacket};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpSocket, UdpSocket};
use tokio::sync::mpsc;
use tokio::time::timeout;

const OPTIONS: &[u8] = b"OPTIONS sip:probe SIP/2.0\r\n\
Via: SIP/2.0/UDP 127.0.0.2;branch=z9hG4bK-block\r\n\
From: <sip:scanner@127.0.0.2>;tag=1\r\n\
To: <sip:probe@127.0.0.1>\r\n\
Call-ID: ingress-block@host\r\n\
CSeq: 1 OPTIONS\r\n\
Content-Length: 0\r\n\r\n";

fn scanner() -> IpAddr {
    "127.0.0.2".parse().unwrap()
}

fn block_scanner() {
    let scanner = scanner();
    set_ingress_block(Some(Arc::new(move |ip| ip == scanner)));
}

async fn nothing_arrives(rx: &mut mpsc::Receiver<InboundPacket>) {
    assert!(
        timeout(Duration::from_millis(300), rx.recv())
            .await
            .is_err(),
        "a blocked source's message reached the channel"
    );
}

async fn arrives_from_scanner(rx: &mut mpsc::Receiver<InboundPacket>) {
    let packet = timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("a message within 2s")
        .expect("the channel is open");
    assert_eq!(packet.peer().ip(), scanner());
    assert_eq!(packet.payload().as_ref(), OPTIONS);
}

// One test: the block is process-wide.
#[tokio::test]
async fn a_blocked_source_is_refused_on_udp_and_tcp_until_unblocked() {
    // UDP.
    let server = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let server_addr = server.local_addr().unwrap();
    let (tx, mut rx) = mpsc::channel::<InboundPacket>(8);
    tokio::spawn(run_udp(server, tx));
    let client = UdpSocket::bind(SocketAddr::new(scanner(), 0))
        .await
        .unwrap();

    block_scanner();
    client.send_to(OPTIONS, server_addr).await.unwrap();
    nothing_arrives(&mut rx).await;

    set_ingress_block(None);
    client.send_to(OPTIONS, server_addr).await.unwrap();
    arrives_from_scanner(&mut rx).await;

    // TCP.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let tcp_addr = listener.local_addr().unwrap();
    drop(listener); // run_tcp binds the same port
    let (tx, mut rx) = mpsc::channel::<InboundPacket>(8);
    tokio::spawn(async move {
        let _ = run_tcp(&tcp_addr.to_string(), tx).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let connect = || async {
        let socket = TcpSocket::new_v4().unwrap();
        socket.bind(SocketAddr::new(scanner(), 0)).unwrap();
        socket.connect(tcp_addr).await.expect("connect")
    };

    block_scanner();
    let mut stream = connect().await;
    // Hung up on at accept: the read sees the close (or a reset), never data.
    let mut buf = [0u8; 64];
    let read = timeout(Duration::from_secs(2), stream.read(&mut buf))
        .await
        .expect("the server closes a blocked source's connection");
    assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
    let _ = stream.write_all(OPTIONS).await;
    nothing_arrives(&mut rx).await;

    set_ingress_block(None);
    let mut stream = connect().await;
    stream.write_all(OPTIONS).await.unwrap();
    arrives_from_scanner(&mut rx).await;
}
