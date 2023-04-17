use std::{fmt::Debug, io};

use chrono::{serde::ts_milliseconds, DateTime, Utc};
use opentelemetry::trace::TraceContextExt;
use serde::Serialize;
use serde_with::{serde_as, DisplayFromStr};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Record};
use tracing::{Event, Id, Level, Metadata, Subscriber};
use tracing_subscriber::{fmt::MakeWriter, layer::Context, registry::LookupSpan, Layer};

pub struct JsonLogLayer<W> {
    make_writer: W,
}

impl<W> JsonLogLayer<W>
where
    W: for<'a> MakeWriter<'a> + 'static,
{
    pub fn new(make_writer: W) -> JsonLogLayer<W> {
        JsonLogLayer { make_writer }
    }
}

impl<S, W> Layer<S> for JsonLogLayer<W>
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    W: for<'a> MakeWriter<'a> + 'static,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        if !is_root(attrs.metadata()) {
            return;
        }

        let Some(span) = ctx.span(id) else { return };

        let mut root_span_entry = SpanEntry::default();
        attrs.record(&mut root_span_entry);

        span.extensions_mut().insert(root_span_entry);
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut extensions = span.extensions_mut();
        let Some(root_span_entry) = extensions.get_mut::<SpanEntry>() else { return };

        values.record(root_span_entry);
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        if event.metadata().fields().field("message").is_none() {
            return;
        }
        let Some(span) = ctx.lookup_current() else { return };
        let Some(root_span) = span.scope().find(|span| is_root(span.metadata())) else { return };
        let extensions = root_span.extensions();
        let Some(root_span_entry) = extensions.get::<SpanEntry>() else { return };
        let mut log_entry = LogEntry::new(*event.metadata().level());
        let trace_id = root_span_entry.trace_id.or_else(|| {
            let span = event
                .parent()
                .and_then(|id| ctx.span(id))
                .or_else(|| ctx.lookup_current())?;
            let extensions = span.extensions();
            let otel_data = extensions.get::<tracing_opentelemetry::OtelData>()?;
            let trace_id = otel_data
                .builder
                .trace_id
                .unwrap_or_else(|| otel_data.parent_cx.span().span_context().trace_id());
            // OpenTelemetryではTraceIdはu128だがDataDogではu64なので変換する
            let trace_id = u128::from_be_bytes(trace_id.to_bytes()) as u64;
            Some(trace_id)
        });
        let span_id = root_span_entry
            .span_id
            .unwrap_or_else(|| root_span.id().into_u64());

        log_entry.trace_id = trace_id.as_ref();
        log_entry.span_id = Some(&span_id);

        event.record(&mut log_entry);
        let _ = write_json_line(self.make_writer.make_writer(), &log_entry);
    }
}

fn is_root(metadata: &Metadata<'_>) -> bool {
    metadata.name() == "root"
}

fn write_json_line(mut w: impl io::Write, entry: impl Serialize) -> io::Result<()> {
    let Ok(mut buf) = serde_json::ser::to_vec(&entry) else { return Ok(()) };
    buf.append(&mut vec![b'\n']);
    w.write_all(&buf)
}

// [属性とエイリアス設定](https://docs.datadoghq.com/ja/logs/log_configuration/attributes_naming_convention/)
#[serde_as]
#[derive(Serialize)]
struct LogEntry<'a> {
    #[serde_as(as = "DisplayFromStr")]
    level: Level,
    message: String,
    #[serde(
        rename(serialize = "dd.trace_id"),
        skip_serializing_if = "Option::is_none"
    )]
    trace_id: Option<&'a u64>,
    #[serde(
        rename(serialize = "dd.span_id"),
        skip_serializing_if = "Option::is_none"
    )]
    span_id: Option<&'a u64>,
    #[serde(with = "ts_milliseconds")]
    timestamp: DateTime<Utc>,
    ts: String,
}

impl<'a> LogEntry<'a> {
    fn new(level: Level) -> LogEntry<'a> {
        let now = Utc::now();
        LogEntry {
            level,
            message: String::new(),
            trace_id: None,
            span_id: None,
            timestamp: now,
            ts: now.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        }
    }
}

impl Visit for LogEntry<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        match field.name() {
            "message" => {
                self.message = format!("{:?}", value);
            }
            _ => {}
        }
    }
}

#[derive(Default, Serialize)]
struct SpanEntry {
    trace_id: Option<u64>,
    span_id: Option<u64>,
}

impl Visit for SpanEntry {
    fn record_debug(&mut self, _field: &Field, _value: &dyn Debug) {}

    fn record_u128(&mut self, field: &Field, value: u128) {
        if field.name() == "dd.trace_id" {
            self.trace_id = Some(value as u64);
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == "dd.span_id" {
            self.span_id = Some(value);
        }
    }
}
