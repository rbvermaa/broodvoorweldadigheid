#![forbid(unsafe_code)]

use notify::{
    Email, EmailKind, EmailSender, OutgoingEmail, SendError, SmtpConfig, SmtpSender, TlsMode,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// A one-connection SMTP server on 127.0.0.1 that answers RCPT with `rcpt_reply` and
/// returns everything the client said.
async fn fake_relay(rcpt_reply: &'static str) -> (u16, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let (read, mut write) = socket.into_split();
        let mut lines = BufReader::new(read).lines();
        let mut said = String::new();
        let mut in_data = false;
        write.write_all(b"220 fake ESMTP\r\n").await.unwrap();
        while let Some(line) = lines.next_line().await.unwrap() {
            said.push_str(&line);
            said.push('\n');
            let reply = if in_data {
                if line != "." {
                    continue;
                }
                in_data = false;
                "250 queued"
            } else {
                let command = line.get(..4).unwrap_or_default().to_ascii_uppercase();
                match command.as_str() {
                    "EHLO" | "HELO" | "MAIL" | "RSET" | "NOOP" => "250 OK",
                    "RCPT" => rcpt_reply,
                    "DATA" => {
                        in_data = true;
                        "354 go ahead"
                    }
                    "QUIT" => "221 bye",
                    _ => "500 what",
                }
            };
            write
                .write_all(format!("{reply}\r\n").as_bytes())
                .await
                .unwrap();
            if reply.starts_with("250 queued") || !reply.starts_with(['2', '3']) {
                break;
            }
        }
        said
    });
    (port, server)
}

fn sender(port: u16) -> SmtpSender {
    let config = SmtpConfig::from_lookup(
        |key| {
            Some(match key {
                "SMTP_HOST" => "127.0.0.1".to_owned(),
                "SMTP_PORT" => port.to_string(),
                "SMTP_TLS" => "none".to_owned(),
                "MAIL_FROM" => {
                    "Brood voor Weldadigheid <bakker@broodvoorweldadigheid.nl>".to_owned()
                }
                _ => return None,
            })
        },
        |_| Err("no files".to_owned()),
    )
    .unwrap();
    assert_eq!(config.tls, TlsMode::None);
    SmtpSender::new(&config).unwrap()
}

fn outgoing() -> OutgoingEmail {
    OutgoingEmail {
        id: "0b5e7c2a-1111-4222-8333-944455556666".to_owned(),
        date: "2026-12-01T08:00:00Z".parse().unwrap(),
        email: Email {
            kind: EmailKind::PickupReminder,
            to: "sanne@example.org".to_owned(),
            subject: "Ophalen: je bestelling 2026-0042".to_owned(),
            text: "Hoi Sanne,\n".to_owned(),
            html: "<p>Hoi Sanne,</p>".to_owned(),
            list_unsubscribe: None,
            reply_to: None,
        },
    }
}

#[tokio::test]
async fn sends_over_smtp() {
    let (port, server) = fake_relay("250 OK").await;
    let sender = sender(port);
    sender.send(&outgoing()).await.expect("sent");
    let said = server.await.unwrap();
    assert!(
        said.starts_with("EHLO broodvoorweldadigheid.nl\n"),
        "{said}"
    );
    assert!(
        said.contains("MAIL FROM:<bakker@broodvoorweldadigheid.nl>"),
        "{said}"
    );
    assert!(said.contains("RCPT TO:<sanne@example.org>"), "{said}");
    assert!(
        said.contains("Subject: Ophalen: je bestelling 2026-0042"),
        "{said}"
    );
    assert!(
        said.contains(
            "Message-ID: <0b5e7c2a-1111-4222-8333-944455556666@broodvoorweldadigheid.nl>"
        ),
        "{said}"
    );
}

#[tokio::test]
async fn a_rejected_recipient_is_permanent() {
    let (port, server) = fake_relay("550 5.1.1 no such user").await;
    let result = sender(port).send(&outgoing()).await;
    assert!(matches!(result, Err(SendError::Permanent(_))), "{result:?}");
    server.await.unwrap();
}

#[tokio::test]
async fn a_sender_policy_refusal_is_temporary_so_a_misconfigured_relay_keeps_the_queue() {
    let (port, server) = fake_relay("550 5.7.1 relaying denied for this sender").await;
    let result = sender(port).send(&outgoing()).await;
    assert!(matches!(result, Err(SendError::Transient(_))), "{result:?}");
    server.await.unwrap();
}

#[tokio::test]
async fn a_busy_relay_is_temporary() {
    let (port, server) = fake_relay("451 4.3.0 try again later").await;
    let result = sender(port).send(&outgoing()).await;
    assert!(matches!(result, Err(SendError::Transient(_))), "{result:?}");
    server.await.unwrap();
}

#[tokio::test]
async fn a_refused_login_is_temporary_so_the_queue_survives_a_wrong_password() {
    let (port, server) = fake_relay("535 5.7.8 authentication failed").await;
    let result = sender(port).send(&outgoing()).await;
    assert!(matches!(result, Err(SendError::Transient(_))), "{result:?}");
    server.await.unwrap();
}

#[tokio::test]
async fn a_relay_that_is_down_is_temporary() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let result = sender(port).send(&outgoing()).await;
    assert!(matches!(result, Err(SendError::Transient(_))), "{result:?}");
}

/// Serves every connection like a normal relay, except that on the first one the reply
/// to the end of DATA comes only after `delay`.
async fn slow_first_relay(delay: Duration) -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&accepted);
    tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let first = count.fetch_add(1, Ordering::SeqCst) == 0;
            tokio::spawn(async move {
                let (read, mut write) = socket.into_split();
                let mut lines = BufReader::new(read).lines();
                let mut in_data = false;
                write.write_all(b"220 fake ESMTP\r\n").await.ok();
                while let Ok(Some(line)) = lines.next_line().await {
                    let reply = if in_data {
                        if line != "." {
                            continue;
                        }
                        in_data = false;
                        if first {
                            tokio::time::sleep(delay).await;
                        }
                        "250 queued"
                    } else {
                        match line
                            .get(..4)
                            .unwrap_or_default()
                            .to_ascii_uppercase()
                            .as_str()
                        {
                            "DATA" => {
                                in_data = true;
                                "354 go ahead"
                            }
                            "QUIT" => "221 bye",
                            _ => "250 OK",
                        }
                    };
                    if write
                        .write_all(format!("{reply}\r\n").as_bytes())
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });
        }
    });
    (port, accepted)
}

#[tokio::test]
async fn a_send_past_its_deadline_is_temporary_and_its_connection_is_not_reused() {
    let (port, accepted) = slow_first_relay(Duration::from_millis(500)).await;
    let sender = sender(port).with_send_deadline(Duration::from_millis(200));
    let result = sender.send(&outgoing()).await;
    assert!(matches!(result, Err(SendError::Transient(_))), "{result:?}");
    tokio::time::sleep(Duration::from_millis(600)).await;
    sender.send(&outgoing()).await.expect("the next send works");
    assert_eq!(accepted.load(Ordering::SeqCst), 2, "a fresh connection");
}
