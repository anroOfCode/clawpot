#[allow(clippy::all, clippy::pedantic)]
mod inner {
    tonic::include_proto!("clawpot.agent.v1");
}
pub use inner::*;
