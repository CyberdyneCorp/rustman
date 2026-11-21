//! Parser-differential inputs: each protocol's policy URL names exactly
//! the host and port its connector dials.

use super::*;

fn policy(endpoint: Result<Endpoint, MessagingError>) -> String {
    endpoint.map_or_else(|e| format!("error: {e}"), |e| e.policy_url())
}

fn amqp_policy(broker: &str) -> String {
    match amqp(broker) {
        Ok((endpoint, uri)) => {
            assert_eq!(uri.authority.host, endpoint.host, "{broker}");
            assert_eq!(uri.authority.port, endpoint.port, "{broker}");
            endpoint.policy_url()
        }
        Err(e) => format!("error: {e}"),
    }
}

fn refused(text: &str) -> bool {
    text.starts_with("error: ")
}

#[test]
fn backslashes_control_characters_and_whitespace_are_refused_for_every_protocol() {
    // Regression: `ws://evil.example\@allowed.example/mqtt` was checked as
    // allowed.example but connected to evil.example.
    for broker in [
        "ws://evil.example\\@allowed.example/mqtt",
        "wss://evil.example\\@allowed.example/mqtt",
        "mqtt://evil.example\\@allowed.example",
        "mqtt://allowed.example\t",
        "mqtt://allowed.example /x",
        "tcp://allowed.example\n",
        "",
    ] {
        assert!(refused(&policy(mqtt(broker))), "{broker}");
    }
    for broker in [
        "amqp://evil.example\\@allowed.example/%2f",
        "amqps://allowed.example\r/",
        "amqp://allowed .example",
    ] {
        assert!(refused(&amqp_policy(broker)), "{broker}");
    }
    for broker in [
        "evil.example\\@allowed.example:9092",
        "allowed.example:9092 ",
        "a\u{7f}:1",
    ] {
        assert!(refused(&policy(kafka(broker))), "{broker}");
    }
}

#[test]
fn the_password_may_hold_an_at_sign() {
    assert_eq!(
        policy(mqtt("mqtt://app:p@ss@allowed.example:1884")),
        "http://allowed.example:1884"
    );
    assert_eq!(
        policy(mqtt("ws://app:p@ss@allowed.example/mqtt")),
        "http://allowed.example:80"
    );
    assert_eq!(
        amqp_policy("amqp://app:p@ss@allowed.example/%2f"),
        "http://allowed.example:5672"
    );
    let err = policy(kafka("app:p@ss@allowed.example:9092"));
    assert_eq!(
        err,
        "error: kafka: invalid broker '«invalid broker»': expected host:port"
    );
}

#[test]
fn ipv6_literals_keep_their_brackets_in_urls_only() {
    let e = mqtt("mqtt://[::1]:1884").unwrap();
    assert_eq!(
        (e.host.as_str(), e.address()),
        ("::1", "[::1]:1884".to_owned())
    );
    assert_eq!(policy(mqtt("wss://[::1]/mqtt")), "https://[::1]:443");
    assert_eq!(
        mqtt("ws://[::1]:9001/m").unwrap().ws_url.unwrap(),
        "ws://[::1]:9001/m"
    );
    assert_eq!(amqp_policy("amqps://[::1]"), "https://[::1]:5671");
    assert_eq!(kafka("[::1]:9093").unwrap().address(), "[::1]:9093");
    assert_eq!(policy(kafka("[::1]")), "http://[::1]:9092");
}

#[test]
fn percent_encoded_hosts_are_decoded_once_or_refused() {
    // ws is a special scheme: the URL parser decodes the host, and both
    // sides use the decoded form.
    let e = mqtt("ws://allowed%2Eexample/mqtt").unwrap();
    assert_eq!(e.host, "allowed.example");
    assert_eq!(e.ws_url.as_deref(), Some("ws://allowed.example/mqtt"));
    // lapin decodes an AMQP host; the endpoint and the dialed URI agree.
    assert_eq!(
        amqp_policy("amqp://allowed%2eexample"),
        "http://allowed.example:5672"
    );
    // An opaque host keeps its escapes and is refused.
    assert!(refused(&policy(mqtt("mqtt://allowed%2eexample"))));
    assert!(refused(&policy(kafka("allowed%2eexample:9092"))));
    assert!(refused(&amqp_policy("amqp://evil%2fx")));
}

#[test]
fn trailing_dots_and_case_are_normalized_for_both_sides() {
    assert_eq!(
        policy(mqtt("MQTT://Allowed.Example.:1883")),
        "http://allowed.example:1883"
    );
    assert_eq!(
        kafka("Allowed.Example.:9092").unwrap().address(),
        "allowed.example:9092"
    );
    let (endpoint, uri) = amqp("amqp://allowed.example./").unwrap();
    assert_eq!(
        (endpoint.host.as_str(), uri.authority.host.as_str()),
        ("allowed.example", "allowed.example")
    );
    assert!(refused(&policy(mqtt("mqtt://allowed..example"))));
}

#[test]
fn schemes_choose_the_transport_and_default_port() {
    let cases = [
        ("mqtt://localhost", Transport::Tcp, 1883),
        ("tcp://localhost", Transport::Tcp, 1883),
        ("localhost:1884", Transport::Tcp, 1884),
        ("mqtts://broker.example", Transport::Tls, 8883),
        ("ssl://broker.example", Transport::Tls, 8883),
        ("ws://broker.example/mqtt", Transport::Ws, 80),
        ("wss://broker.example:8443/mqtt", Transport::Wss, 8443),
    ];
    for (broker, transport, port) in cases {
        let e = mqtt(broker).unwrap();
        assert_eq!((e.transport, e.port), (transport, port), "{broker}");
    }
    assert_eq!(policy(mqtt("ssl://h")), "https://h:8883");
    assert_eq!(
        policy(mqtt("http://h")),
        "error: mqtt: invalid broker 'http://«invalid broker»': unsupported scheme 'http'"
    );
    assert!(refused(&policy(kafka("h:9092/path"))));
    assert!(refused(&policy(kafka("h:9092?x=1"))));
}

#[test]
fn websocket_urls_carry_no_credentials() {
    let e = mqtt("wss://app:pw@broker.example/mqtt?x=1").unwrap();
    assert_eq!(e.ws_url.as_deref(), Some("wss://broker.example/mqtt?x=1"));
    assert_eq!(mqtt("mqtt://h").unwrap().ws_url, None);
}

#[test]
fn shown_brokers_hide_their_userinfo() {
    assert_eq!(
        without_userinfo("tcp://app:pw@h:1/x@y"),
        "tcp://«redacted:userinfo»@h:1/x@y"
    );
    assert_eq!(
        without_userinfo("app:pw@h:9092"),
        "«redacted:userinfo»@h:9092"
    );
    assert_eq!(without_userinfo("mqtt://h/a@b"), "mqtt://h/a@b");
}

/// Hosts whose last label is numeric but which are not strict
/// dotted-decimal IPv4: URL parsers read them as IPv4 (octal, hex or
/// short forms), resolvers may not.
const NUMERIC_HOSTS: [&str; 10] = [
    "0177.0.0.1",
    "1.2.3.0x",
    "2130706433",
    "0x7f.1",
    "012.0.0.01",
    "0177.0.0.01",
    "127.1",
    "0X7F.0.0.1",
    "127.0.0.0x1",
    "1.2.3.4.5",
];

#[test]
fn numeric_hosts_must_be_strict_dotted_decimal_for_every_protocol() {
    // Regression: `0177.0.0.1` on a non-special scheme was kept as a DNS
    // name and dialed through getaddrinfo (decimal 177.0.0.1 on macOS)
    // while the policy read it as 127.0.0.1.
    let mut accepted = Vec::new();
    for host in NUMERIC_HOSTS {
        let mut check = |broker: String, shown: String| {
            if !refused(&shown) {
                accepted.push(format!("{broker} -> {shown}"));
            }
        };
        let broker = format!("{host}:9092");
        check(broker.clone(), policy(kafka(&broker)));
        for scheme in ["mqtt://", "tcp://", "mqtts://", "ssl://", ""] {
            let broker = format!("{scheme}{host}:1883");
            check(broker.clone(), policy(mqtt(&broker)));
        }
        for scheme in ["amqp", "amqps"] {
            let broker = format!("{scheme}://{host}/%2f");
            check(broker.clone(), amqp_policy(&broker));
        }
    }
    assert!(accepted.is_empty(), "{accepted:#?}");
}

#[test]
fn strict_ipv4_hosts_are_dialed_as_the_policy_reads_them() {
    assert_eq!(policy(kafka("127.0.0.1.:9092")), "http://127.0.0.1:9092");
    assert_eq!(policy(mqtt("tcp://10.0.0.1")), "http://10.0.0.1:1883");
    assert_eq!(amqp_policy("amqps://10.0.0.1"), "https://10.0.0.1:5671");
    // ws is a special scheme: the URL parser itself normalizes the IPv4
    // forms, and the dialed and checked host are its result.
    let e = mqtt("ws://0177.0.0.1/mqtt").unwrap();
    assert_eq!(e.host, "127.0.0.1");
    assert_eq!(e.ws_url.as_deref(), Some("ws://127.0.0.1/mqtt"));
    // A numeric label before a name is a DNS name for every parser.
    assert_eq!(
        policy(kafka("0177.example:9092")),
        "http://0177.example:9092"
    );
    assert_eq!(
        policy(mqtt("mqtt://1.0x7f.example")),
        "http://1.0x7f.example:1883"
    );
}

#[test]
fn invalid_broker_errors_never_echo_the_broker() {
    // Regression: the userinfo was cut at the first '/', so a password
    // holding one was echoed in the error.
    let err = amqp_policy("amqp://guest:SLASH/PW@rabbit/");
    assert!(
        err.starts_with("error: amqp: invalid broker 'amqp://«invalid broker»': "),
        "{err}"
    );
    assert!(!err.contains("SLASH"), "{err}");
    for broker in [
        "mqtt://app:Q?PW@h",
        "mqtts://app:H#PW@h",
        "ws://app:S/PW@h/mqtt",
    ] {
        let err = policy(mqtt(broker));
        assert!(refused(&err) && !err.contains("PW"), "{err}");
        assert!(err.contains("://«invalid broker»'"), "{err}");
    }
    let err = policy(kafka("app:K/PW@h:9092"));
    assert!(
        err.starts_with("error: kafka: invalid broker '«invalid broker»': ") && !err.contains("PW"),
        "{err}"
    );
}
