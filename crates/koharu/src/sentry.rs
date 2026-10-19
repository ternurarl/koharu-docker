use sentry::{ClientOptions, IntoDsn};
use tracing_subscriber::registry::LookupSpan;

pub fn initialize() -> sentry::ClientInitGuard {
    sentry::init(ClientOptions {
        dsn: option_env!("SENTRY_DSN")
            .into_dsn()
            .expect("invalid SENTRY_DSN environment variable"),
        release: sentry::release_name!(),
        // Never ship request payloads, URLs, or breadcrumbs: a translation
        // error can name a provider endpoint, and a shared host must not hand
        // that to a third party. Build public images without SENTRY_DSN at all.
        send_default_pii: false,
        sample_rate: 0.1,
        auto_session_tracking: true,
        ..Default::default()
    })
}

pub fn tracing_layer<S>() -> impl tracing_subscriber::Layer<S>
where
    S: tracing::Subscriber + for<'span> LookupSpan<'span>,
{
    sentry::integrations::tracing::layer()
}
