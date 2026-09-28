use super::*;

const WORKER: &str = r#"
if (typeof self.postMessage === 'function') {
  const w = new URL(location.href).searchParams.get('w');
  fetch('/fidelity-network?' + new URLSearchParams({w, from:'dedicated'}))
    .then(() => self.postMessage({ua:navigator.userAgent}));
}
self.addEventListener('connect', event => {
  const port = event.ports[0];
  port.onmessage = async message => {
    await fetch('/fidelity-network?' + new URLSearchParams(message.data));
    port.postMessage({ua:navigator.userAgent});
  };
});
self.addEventListener('install', event => self.skipWaiting());
self.addEventListener('activate', event => event.waitUntil(self.clients.claim()));
self.addEventListener('message', async event => {
  const data = event.data;
  await fetch('/fidelity-network?' + new URLSearchParams(data));
  event.ports[0].postMessage({ua:navigator.userAgent});
});
"#;

const WORKER_PAGE: &str = r#"<!doctype html><meta name=viewport content="width=device-width,initial-scale=1"><script>
(async () => {
  const w = innerWidth;
  const ask = (worker, from) => new Promise((resolve, reject) => {
    const channel = new MessageChannel();
    worker.addEventListener('error', event => reject(new Error(from + ': ' + event.message)));
    channel.port1.onmessage = event => { channel.port1.close(); resolve(event.data.ua); };
    worker.postMessage({w, from}, [channel.port2]);
  });
  await fetch('/fidelity-network?' + new URLSearchParams({w, from:'page'}));
  const worker = new Worker('/fidelity-worker.js?w=' + w);
  const dedicated = await new Promise((resolve, reject) => {
    worker.onmessage = event => resolve(event.data.ua);
    worker.onerror = event => reject(new Error(event.message));
  });
  worker.terminate();
  const sharedWorker = new SharedWorker('/fidelity-shared.js?w=' + w);
  const shared = await new Promise((resolve, reject) => {
    sharedWorker.port.onmessage = event => resolve(event.data.ua);
    sharedWorker.onerror = event => reject(new Error(event.message));
    sharedWorker.port.postMessage({w, from:'shared'});
  });
  sharedWorker.port.close();
  const registration = await navigator.serviceWorker.register('/fidelity-service.js?w=' + w, {scope:'/scope-' + w + '/'});
  const service = registration.installing || registration.waiting || registration.active;
  if (service.state !== 'activated') await new Promise(resolve => {
    service.addEventListener('statechange', () => { if (service.state === 'activated') resolve(); });
  });
  const serviceUA = await ask(service, 'service');
  await fetch('/event?' + new URLSearchParams({kind:'fidelity-workers', w, page:navigator.userAgent, dedicated, shared, service:serviceUA}));
})().catch(error => fetch('/event?' + new URLSearchParams({kind:'fidelity-error', error:String(error), w:innerWidth})));
</script>"#;

fn worker_fixture() -> Fixture {
    Fixture::start(|request, _| {
        if request.path.starts_with("/fidelity-worker.js")
            || request.path.starts_with("/fidelity-shared.js")
            || request.path.starts_with("/fidelity-service.js")
        {
            Reply::JavaScript(WORKER.into())
        } else {
            Reply::Html {
                body: if request.path == "/workers" {
                    WORKER_PAGE.into()
                } else {
                    "<!doctype html><title>owned fidelity fixture</title>".into()
                },
                delay: Duration::ZERO,
                cookie: None,
            }
        }
    })
}

fn assert_headed_ua(ua: &str) {
    assert!(
        ua.contains(" Chrome/") && !ua.contains("HeadlessChrome"),
        "{ua}"
    );
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_user_agent_matches_in_pages_workers_and_network_requests() {
    let fixture = worker_fixture();
    let live = Live::start(workspace(fixture.url("/workers")));
    live.wait("worker pages", Duration::from_secs(30), |s| {
        loaded(s, &fixture, "/workers")
    });
    assert!(
        fixture.wait_for(Duration::from_secs(15), |f| events(f, "fidelity-workers")
            .len()
            == 3),
        "reports: {:?}; errors: {:?}; requests: {:?}",
        events(&fixture, "fidelity-workers"),
        events(&fixture, "fidelity-error"),
        fixture
            .requests()
            .iter()
            .map(|r| &r.path)
            .collect::<Vec<_>>()
    );
    let reports = events(&fixture, "fidelity-workers");
    for report in &reports {
        assert_headed_ua(&report["page"]);
        assert_eq!(report["dedicated"], report["page"], "{report:?}");
        assert_eq!(report["shared"], report["page"], "{report:?}");
        assert_eq!(report["service"], report["page"], "{report:?}");
        for from in ["page", "dedicated", "shared", "service"] {
            let path = format!("/fidelity-network?w={}&from={from}", report["w"]);
            let requests: Vec<_> = fixture
                .requests()
                .into_iter()
                .filter(|r| r.path == path)
                .collect();
            assert_eq!(requests.len(), 1, "{path}: {requests:?}");
            assert_eq!(
                requests[0].user_agent.as_deref(),
                Some(report["page"].as_str()),
                "{requests:?}"
            );
            if from == "page" {
                // Worker fetches have a separate client-hint policy; their
                // User-Agent still has to agree with the page and worker JS.
                assert!(
                    requests[0]
                        .client_hint_brands
                        .as_deref()
                        .is_some_and(|brands| brands.contains("Chromium")),
                    "{requests:?}"
                );
            }
        }
    }
    assert_eq!(
        count(&fixture, "/workers"),
        3,
        "discovery must never load the workspace URL"
    );
    live.close();
}

#[test]
#[ignore = "requires an installed CDP browser"]
fn live_capture_uses_the_aligned_browser_without_discovery_navigation() {
    let fixture = worker_fixture();
    let root = profile_root();
    let output = tempfile::tempdir().unwrap();
    let outcome = crate::capture::run(
        &workspace(fixture.url("/capture")),
        &BrowserOptions {
            executable: test_browser(),
            headless: true,
            profile_root: Some(root.path().to_owned()),
            cancel: Cancellation::new(),
        },
        output.path(),
        crate::capture::Plan::default(),
    );
    let report = outcome.result.unwrap();
    assert_eq!(report.frames.len(), 3);
    let requests: Vec<_> = fixture
        .requests()
        .into_iter()
        .filter(|r| r.path == "/capture")
        .collect();
    assert_eq!(
        requests.len(),
        3,
        "discovery must never load the workspace URL"
    );
    for request in requests {
        assert_headed_ua(request.user_agent.as_deref().unwrap());
        assert!(
            request
                .client_hint_brands
                .as_deref()
                .is_some_and(|brands| brands.contains("Chromium")),
            "{request:?}"
        );
    }
    crate::test_support::assert_cleaned_up(root.path(), &outcome.diagnostics.processes);
}
