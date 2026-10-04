//! Le backfill de la recherche (#XXX, lot 3a) : le moteur reprend à son
//! curseur et n'indexe jamais deux fois le même mail. Pur : `run_once` prend
//! une liste de mails et un chemin de curseur. Le câblage temps réel — l'index
//! qui se remplit à chaque poll — est testé ici même, à la frontière du
//! processus (`a_polled_mail_reaches_the_index`), avec la pile.

mod support;

use anyhow::Result;
use serde_json::Value;
use twalk_collector::jmap::{Mail, Person};
use twalk_test_harness::{ensure_stack, poll_until};

fn person(name: &str, email: &str) -> Person {
    Person {
        name: Some(name.to_owned()),
        email: email.to_owned(),
    }
}

fn a_mail(id: &str, from_email: &str) -> Mail {
    Mail {
        id: id.to_owned(),
        mailbox_ids: vec!["inbox".to_owned()],
        received_at: "2026-09-01T10:00:00Z".to_owned(),
        from: person("Alice", from_email),
        to: vec![person("Owner", "michel@example.com")],
        cc: vec![],
        subject: format!("Sujet {id}"),
        body: format!("Corps {id}"),
        message_id: Some(format!("<{id}@example.org>")),
        in_reply_to: None,
        references: vec![],
        attachments: vec![],
        auto_submitted: None,
        list_id: None,
        list_unsubscribe: None,
        precedence: None,
        has_itip_part: false,
    }
}

/// Un passage indexe deux mails, un second passage sur les mêmes mails n'écrit
/// rien : le curseur a retenu. Et l'index le confirme — deux documents, pas
/// quatre (§3.3, §8.3).
#[test]
fn the_backfill_resumes_from_its_cursor() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("twalk-backfill-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let index_dir = dir.join("index");
    let cursor = index_dir.join("mail-linagora.cursor.json");
    let owner = twalk_collector::owner::Owner::new("michel@example.com", Vec::<String>::new());
    let mails = vec![
        a_mail("m1", "alice@example.org"),
        a_mail("m2", "bob@example.org"),
    ];

    let first = twalk_collector::backfill::run_once(
        "mail-linagora",
        "acct",
        &owner,
        &index_dir,
        &cursor,
        &mails,
    )?;
    assert_eq!(2, first, "the first pass indexes both mails");

    let second = twalk_collector::backfill::run_once(
        "mail-linagora",
        "acct",
        &owner,
        &index_dir,
        &cursor,
        &mails,
    )?;
    assert_eq!(0, second, "a resumed pass reindexes nothing: {second}");

    let stored = twalk_collector::search_index::Stored::open_or_create(&index_dir)?;
    assert_eq!(2, stored.document_count(), "two documents, not four");

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// L'indexation temps réel : le mail que le poll lit est dans l'index, sans
/// backfill. Preuve à la frontière, par la jauge `twalk_collector_index_documents`
/// (C25) — l'index n'a pas de route avant T6 —, et par le compte que
/// `Stored::document_count` donne sur l'index réellement écrit sous le
/// répertoire d'état du run.
#[tokio::test]
async fn a_polled_mail_reaches_the_index() -> Result<()> {
    ensure_stack().await?;
    let bus = twalk_test_harness::Bus::connect().await?;
    let run = support::Run::prepare("search-realtime").await?;
    run.authorize().await?;
    run.serve_snapshot(&bus, Vec::new()).await?;
    let (collector, _http_port, metrics_port) = run.start_with_gateway_and_index()?;
    collector.wait_logged("search index opened", 1).await?;

    // Un mail arrive dans l'INBOX du propriétaire, d'un tiers.
    run.deliver_mail("Alice", "alice@example.org", "Sujet temps réel");

    // Le poll l'indexe ; la jauge le dit (elle vaut 1, pas 0).
    support::wait_metric(metrics_port, "twalk_collector_index_documents", 1.0).await?;

    // Et l'index sur disque le confirme, lu par un autre `Stored` que celui du
    // processus : le document est bien commité, pas seulement compté.
    let stored = twalk_collector::search_index::Stored::open_or_create(&run.index_dir())?;
    assert_eq!(
        1,
        stored.document_count(),
        "the polled mail was not committed to the index"
    );

    let _ = collector.stop().await;
    Ok(())
}

/// The service token every run is configured with (`env_with_gateway`).
const SERVICE_TOKEN: &str = "test-service-token";

/// One search, as the Companion Gateway will relay it: the query, the filters,
/// and this collector's service token.
async fn search(
    port: u16,
    token: &str,
    query: &str,
    source: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
) -> Result<(u16, Value)> {
    let mut pairs: Vec<(&str, &str)> = vec![("q", query)];
    if let Some(source) = source {
        pairs.push(("source", source));
    }
    if let Some(from) = from {
        pairs.push(("from", from));
    }
    if let Some(to) = to {
        pairs.push(("to", to));
    }
    let response = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/search"))
        .query(&pairs)
        .bearer_auth(token)
        .send()
        .await?;
    let status = response.status().as_u16();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    Ok((status, body))
}

/// Attends que l'endpoint interne réponde : une lecture sans jeton suffit, et
/// elle est comptée comme les autres.
async fn wait_for_endpoint(port: u16) -> Result<()> {
    poll_until(
        || async {
            reqwest::Client::new()
                .get(format!("http://127.0.0.1:{port}/index/status"))
                .send()
                .await
                .ok()
                .map(|_| ())
        },
        "the internal endpoint to answer",
    )
    .await
}

/// Un mail d'un tiers arrive dans l'INBOX, et l'on attend que le poll l'ait
/// indexé : **par la même poignée** que celle que la route lit (C27), jamais
/// par un second `Stored` — deux `Stored` sur un même index divergent, et le
/// test mesurerait alors l'index du test, pas celui du processus. La jauge
/// `twalk_collector_index_documents` est la preuve que le poll a commité.
async fn indexed_mail(
    run: &support::Run,
    metrics_port: u16,
    name: &str,
    from: &str,
    subject: &str,
    expected: f64,
) -> Result<()> {
    run.deliver_mail(name, from, subject);
    support::wait_metric(metrics_port, "twalk_collector_index_documents", expected).await
}

/// Le refus quand l'index n'est pas configuré : `503 index_not_configured`,
/// et l'endpoint ne tombe pas.
#[tokio::test]
async fn search_without_an_index_is_a_structured_refusal() -> Result<()> {
    ensure_stack().await?;
    let bus = twalk_test_harness::Bus::connect().await?;
    let run = support::Run::prepare("search-refusal").await?;
    run.authorize().await?;
    run.serve_snapshot(&bus, Vec::new()).await?;
    // Pas de `COLLECTOR_INDEX_KEY_FILE` : l'index est désactivé.
    let (collector, port) = run.start_with_gateway_and_http()?;
    collector.wait_logged("search index is disabled", 1).await?;
    wait_for_endpoint(port).await?;

    let (status, answer) = search(port, SERVICE_TOKEN, "hello", None, None, None).await?;
    assert_eq!(status, 503, "{answer}");
    assert_eq!("index_not_configured", answer["error"], "{answer}");
    let _ = collector.stop().await;
    Ok(())
}

/// Un appelant sans le jeton de service est refusé : `401 unauthenticated`,
/// comme les autres lectures de cet endpoint.
#[tokio::test]
async fn a_search_without_the_service_token_is_refused() -> Result<()> {
    ensure_stack().await?;
    let bus = twalk_test_harness::Bus::connect().await?;
    let run = support::Run::prepare("search-auth").await?;
    run.authorize().await?;
    run.serve_snapshot(&bus, Vec::new()).await?;
    let (collector, port, _metrics) = run.start_with_gateway_and_index()?;
    collector.wait_logged("search index opened", 1).await?;
    wait_for_endpoint(port).await?;

    let (status, answer) = search(port, "not-the-token", "hello", None, None, None).await?;
    assert_eq!(status, 401, "{answer}");
    assert_eq!("unauthenticated", answer["error"], "{answer}");
    let _ = collector.stop().await;
    Ok(())
}

/// Une requête vide est refusée sans faire tomber le service.
#[tokio::test]
async fn an_empty_query_is_refused() -> Result<()> {
    ensure_stack().await?;
    let bus = twalk_test_harness::Bus::connect().await?;
    let run = support::Run::prepare("search-empty").await?;
    run.authorize().await?;
    run.serve_snapshot(&bus, Vec::new()).await?;
    let (collector, port, _metrics) = run.start_with_gateway_and_index()?;
    collector.wait_logged("search index opened", 1).await?;
    wait_for_endpoint(port).await?;

    let (status, answer) = search(port, SERVICE_TOKEN, "", None, None, None).await?;
    assert_eq!(status, 400, "{answer}");
    assert_eq!("invalid_query", answer["error"], "{answer}");
    let _ = collector.stop().await;
    Ok(())
}

/// Une fenêtre inversée est refusée, sans tomber.
#[tokio::test]
async fn a_window_whose_from_is_after_to_is_refused() -> Result<()> {
    ensure_stack().await?;
    let bus = twalk_test_harness::Bus::connect().await?;
    let run = support::Run::prepare("search-window").await?;
    run.authorize().await?;
    run.serve_snapshot(&bus, Vec::new()).await?;
    let (collector, port, _metrics) = run.start_with_gateway_and_index()?;
    collector.wait_logged("search index opened", 1).await?;
    wait_for_endpoint(port).await?;

    let (status, answer) = search(
        port,
        SERVICE_TOKEN,
        "hello",
        None,
        Some("2026-02-01T00:00:00Z"),
        Some("2026-01-01T00:00:00Z"),
    )
    .await?;
    assert_eq!(status, 400, "{answer}");
    assert_eq!("invalid_window", answer["error"], "{answer}");
    let _ = collector.stop().await;
    Ok(())
}

/// Les filtres sont appliqués : un `source` qui ne correspond à rien rend
/// `count: 0` même quand `q` matche un document indexé.
#[tokio::test]
async fn the_source_filter_is_applied() -> Result<()> {
    ensure_stack().await?;
    let bus = twalk_test_harness::Bus::connect().await?;
    let run = support::Run::prepare("search-source").await?;
    run.authorize().await?;
    run.serve_snapshot(&bus, Vec::new()).await?;
    let (collector, port, metrics_port) = run.start_with_gateway_and_index()?;
    collector.wait_logged("search index opened", 1).await?;
    wait_for_endpoint(port).await?;
    // Un mail d'Alice : le poll l'indexe sous `run.mail`, sans filtre il se trouve.
    indexed_mail(&run, metrics_port, "Alice", "alice@example.org", "hello there", 1.0).await?;

    let (status, answer) = search(port, SERVICE_TOKEN, "hello", None, None, None).await?;
    assert_eq!(status, 200, "{answer}");
    assert_eq!(1, answer["count"], "the indexed mail was not found: {answer}");

    // Le même terme, filtré sur une source qui n'existe pas : zéro.
    let (status, answer) =
        search(port, SERVICE_TOKEN, "hello", Some("mail-absent"), None, None).await?;
    assert_eq!(status, 200, "{answer}");
    assert_eq!(0, answer["count"], "the source filter did not apply: {answer}");
    let _ = collector.stop().await;
    Ok(())
}

/// Un correspondant révoqué est retiré des résultats et **compté** : le
/// nombre est rendu, la raison aussi, et jamais le mot du mail (§5.3).
#[tokio::test]
async fn a_revoked_correspondent_is_withheld_from_search_and_counted() -> Result<()> {
    ensure_stack().await?;
    let bus = twalk_test_harness::Bus::connect().await?;
    let run = support::Run::prepare("search-withheld").await?;
    run.authorize().await?;
    // Alice révoquée sur la connexion mail avant le démarrage : le snapshot
    // le dit, le cache le porte.
    run.serve_snapshot(
        &bus,
        vec![run.decided_on_mail("mailto:alice@example.org", "revoked")],
    )
    .await?;
    let (collector, port, metrics_port) = run.start_with_gateway_and_index()?;
    collector.wait_logged("search index opened", 1).await?;
    wait_for_endpoint(port).await?;
    indexed_mail(&run, metrics_port, "Alice", "alice@example.org", "hello from Alice", 1.0).await?;

    let (status, answer) = search(port, SERVICE_TOKEN, "hello", None, None, None).await?;
    assert_eq!(status, 200, "{answer}");
    assert_eq!(0, answer["count"], "the revoked hit is gone: {answer}");
    assert_eq!(1, answer["withheld"], "and the withdrawal is counted");
    assert_eq!("consent", answer["withheld_reason"], "{answer}");
    // Le mot du mail n'est nulle part dans la réponse.
    assert!(!answer.to_string().contains("Alice"), "{answer}");

    // Compté sur sa propre série, à la frontière.
    let text = reqwest::get(format!("http://127.0.0.1:{metrics_port}/metrics"))
        .await?
        .text()
        .await?;
    assert!(
        text.contains("twalk_collector_search_hits_withheld_total{reason=\"consent\"} 1"),
        "the withheld hit was not counted: {text}"
    );
    assert!(
        text.contains("twalk_collector_search_reads_total{outcome=\"served\"} 1"),
        "the served search was not counted: {text}"
    );
    let _ = collector.stop().await;
    Ok(())
}

/// `GET /index/status` : le nombre de documents, sous le même verrou que la
/// recherche (§5.1).
#[tokio::test]
async fn index_status_answers_the_document_count() -> Result<()> {
    ensure_stack().await?;
    let bus = twalk_test_harness::Bus::connect().await?;
    let run = support::Run::prepare("search-status").await?;
    run.authorize().await?;
    run.serve_snapshot(&bus, Vec::new()).await?;
    let (collector, port, metrics_port) = run.start_with_gateway_and_index()?;
    collector.wait_logged("search index opened", 1).await?;
    wait_for_endpoint(port).await?;
    indexed_mail(&run, metrics_port, "Bob", "bob@example.org", "un sujet quelconque", 1.0).await?;

    let answer: Value = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/index/status"))
        .bearer_auth(SERVICE_TOKEN)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(1, answer["documents"], "{answer}");
    let _ = collector.stop().await;
    Ok(())
}
