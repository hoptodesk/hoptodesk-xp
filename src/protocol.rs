
pub mod rendezvous_proto {
    include!(concat!(env!("OUT_DIR"), "/rendezvous.rs"));
}

pub mod message_proto {
    include!(concat!(env!("OUT_DIR"), "/message.rs"));
}

use protobuf::Message;

pub use rendezvous_proto::*;
pub use message_proto::*;

pub fn encode_rendezvous(msg: &RendezvousMessage) -> Vec<u8> {
    msg.write_to_bytes().unwrap_or_default()
}

pub fn decode_rendezvous(data: &[u8]) -> Option<RendezvousMessage> {
    RendezvousMessage::parse_from_bytes(data).ok()
}

pub fn encode_message(msg: &message_proto::Message) -> Vec<u8> {
    msg.write_to_bytes().unwrap_or_default()
}

pub fn decode_message(data: &[u8]) -> Option<message_proto::Message> {
    message_proto::Message::parse_from_bytes(data).ok()
}

pub fn make_register_peer(id: &str) -> RendezvousMessage {
    let mut rp = RegisterPeer::new();
    rp.id = id.to_string();
    rp.serial = 0;

    let mut msg = RendezvousMessage::new();
    msg.set_register_peer(rp);
    msg
}

pub fn make_punch_hole_request(target_id: &str, my_nat: NatType) -> RendezvousMessage {
    let mut phr = PunchHoleRequest::new();
    phr.id = target_id.to_string();
    phr.nat_type = my_nat.into();
    phr.version = env!("CARGO_PKG_VERSION").to_string();

    let mut msg = RendezvousMessage::new();
    msg.set_punch_hole_request(phr);
    msg
}

pub fn make_login_request(my_id: &str, password: &[u8]) -> message_proto::Message {
    let mut lr = LoginRequest::new();
    lr.my_id = my_id.to_string();
    lr.password = password.to_vec().into();
    lr.my_name = hostname();
    lr.my_platform = "Windows".to_string();
    lr.version = env!("CARGO_PKG_VERSION").to_string();

    let mut option = OptionMessage::new();
    let mut decoding = SupportedDecoding::new();
    decoding.ability_vp8 = 1;
    option.supported_decoding = protobuf::MessageField::some(decoding);
    lr.option = protobuf::MessageField::some(option);

    let mut msg = message_proto::Message::new();
    msg.set_login_request(lr);
    msg
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "HopToDesk-XP".to_string())
}
