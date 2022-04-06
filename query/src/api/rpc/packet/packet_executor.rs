use crate::api::rpc::packet::packet_fragment::FragmentPacket;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ExecutorPacket {
    executor: String,
    // receive_executors: Vec<String>,
    fragments_packets: Vec<FragmentPacket>,
}

impl ExecutorPacket {
    pub fn create(executor: String, fragments_packets: Vec<FragmentPacket>) -> ExecutorPacket {
        ExecutorPacket { executor, fragments_packets }
    }
}
