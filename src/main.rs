use axum::{http::header::USER_AGENT, http::Request, routing, Router};
use opentelemetry::trace::TraceContextExt;
use std::sync::Mutex;
use tower::ServiceBuilder;
use tower_http::trace::{MakeSpan, TraceLayer};
use tracing::instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Registry};

mod json_log_layer;
use json_log_layer::JsonLogLayer;

#[tokio::main]
async fn main() {
    setup_tracing();

    let tracing_middleware = ServiceBuilder::new()
        .layer(TraceLayer::new_for_http().make_span_with(MakeRootSpanWithRemote::new()));
    let app = Router::new()
        .route("/greet", routing::get(handler))
        .layer(tracing_middleware);

    axum::Server::bind(&"0.0.0.0:3000".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();
}

#[instrument(skip_all)]
async fn handler() -> &'static str {
    tracing::info!("🦖🦖🦖");
    // NOTE: Spanの親子構造を例示したいのでuse_case()を呼ぶ
    use_case().await
}

#[instrument(skip_all)]
async fn use_case() -> &'static str {
    tracing::info!("🌈🌈🌈");
    "Hello, World!"
}

fn setup_tracing() {
    // clientのヘッダーからtrace_idとspan_idを取得するための設定
    opentelemetry::global::set_text_map_propagator(opentelemetry_datadog::DatadogPropagator::new());

    let tracer = opentelemetry_datadog::new_pipeline()
        .with_service_name("demo")
        .with_agent_endpoint("http://datadog-agent:8126")
        .install_batch(opentelemetry::runtime::Tokio)
        .expect("failed to initialize tracer");
    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    // INFOレベル以上のログのみを出力する
    let filter_layer = EnvFilter::try_new("info").unwrap();

    Registry::default()
        .with(filter_layer)
        .with(otel_layer)
        .with(JsonLogLayer::new(Mutex::new(std::io::stdout())))
        .try_init()
        .expect("Failed to initialize tracing");
}

#[derive(Clone)]
pub struct MakeRootSpanWithRemote {}

impl MakeRootSpanWithRemote {
    pub fn new() -> Self {
        Self {}
    }
}

impl<B> MakeSpan<B> for MakeRootSpanWithRemote {
    fn make_span(&mut self, request: &Request<B>) -> tracing::Span {
        let app_root = tracing::info_span!(
            "root",
            "dd.trace_id" = tracing::field::Empty,
            "dd.span_id" = tracing::field::Empty,

            // for OpenTelemetry
            "otel.name" = %request.uri(),
            "otel.kind" = "server",

            // for Datadog
            "span.type" = "web",
            "span.name" = %request.uri(),
            "http.url" = request.uri().to_string(),
            "http.method" = request.method().to_string(),
            "http.version" = ?request.version(),
            "http.useragent" = request
                .headers()
                .get(USER_AGENT)
                .map(|e| e.to_str().unwrap_or_default()),
            "http.status_code" = tracing::field::Empty, // must be filled with Empty in advance
        );

        let parent_cx = opentelemetry::global::get_text_map_propagator(|propagator| {
            propagator.extract(&opentelemetry_http::HeaderExtractor(request.headers()))
        });

        if parent_cx.span().span_context().is_valid() {
            let trace_id =
                u128::from_be_bytes(parent_cx.span().span_context().trace_id().to_bytes());
            let span_id = u64::from_be_bytes(parent_cx.span().span_context().span_id().to_bytes());

            app_root.set_parent(parent_cx);
            app_root.record("dd.trace_id", &trace_id);
            app_root.record("dd.span_id", &span_id);
        }

        app_root
    }
}
