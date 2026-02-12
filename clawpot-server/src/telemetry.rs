use anyhow::Result;
use opentelemetry::trace::TracerProvider;
use opentelemetry::KeyValue;
use opentelemetry_otlp::SpanExporter;
use opentelemetry_sdk::{
    trace::{BatchSpanProcessor, SdkTracerProvider},
    Resource,
};
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Registry};

/// Initialize the telemetry pipeline with both stdout logging and OTLP export.
///
/// Returns the `SdkTracerProvider` which must be kept alive for the duration of
/// the program and shut down gracefully before exit to flush remaining spans.
///
/// Configuration is via standard OTel environment variables:
/// - `OTEL_EXPORTER_OTLP_ENDPOINT` (default: `http://localhost:4317`)
/// - `RUST_LOG` (default: `info`)
pub fn init_telemetry() -> Result<SdkTracerProvider> {
    let resource = Resource::builder()
        .with_service_name("clawpot-server")
        .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
        .build();

    let exporter = SpanExporter::builder().with_tonic().build()?;

    let provider = SdkTracerProvider::builder()
        .with_resource(resource)
        .with_span_processor(BatchSpanProcessor::builder(exporter).build())
        .build();

    let tracer = provider.tracer("clawpot-server");

    let fmt_layer = fmt::layer().with_target(false).with_level(true);

    let otel_layer = OpenTelemetryLayer::new(tracer);

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    Registry::default()
        .with(filter)
        .with(fmt_layer)
        .with(otel_layer)
        .init();

    Ok(provider)
}
