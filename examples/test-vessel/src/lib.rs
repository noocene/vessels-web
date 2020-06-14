use protocol_mve_transport::ProtocolMveTransport;
use vessels::{runtime::RawAdapter, vessel};

vessel! {
    using RawAdapter<_, _, _, _, ProtocolMveTransport>;
    spawner => {
        "hello there".to_owned()
    }
}
