//! Dynamic (subprocess) entrypoint for the immich plugin.
//!
//! A single-facet `service` plugin: the [`Plugin`](plugin_toolkit::plugin::Plugin)
//! builder registers the [`ServiceBackend`] and emits all the wire dispatch, so
//! the plugin hand-writes no op strings and owns no runtime — it reaches orca
//! only through the socket.
plugin_toolkit::instrument::bootstrap!();

#[allow(unused_imports)]
use immich::tools as _;
use immich::ImmichBackend;
use plugin_toolkit::plugin::Plugin;

fn main() -> plugin_toolkit::anyhow::Result<()> {
    Plugin::named("immich")
        .version(env!("CARGO_PKG_VERSION"))
        .service(ImmichBackend::new("immich"))
        .tools(["immich."])
        .serve()
}
