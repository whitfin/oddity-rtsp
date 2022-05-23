use std::net::SocketAddr;

use oddity_rtsp_protocol::Transport;

use oddity_video::RtpMuxer;

use crate::net::WriterTx;

pub struct Context {
  pub muxer: RtpMuxer,
  pub transport: Transport,
  pub dest: Destination,
}

pub enum Destination {
  Udp(UdpDestination),
  TcpInterleaved(TcpInterleavedDestination),
}

pub struct UdpDestination {
  pub rtp_remote: SocketAddr,
  pub rtcp_remote: SocketAddr,
}

pub struct TcpInterleavedDestination {
  pub tx: WriterTx,
  pub rtp_channel: u8,
  pub rtcp_channel: u8,
}