mod transport;

pub mod session_manager;
pub mod setup;

use std::fmt;

use tokio::select;
use tokio::net;
use tokio::sync::mpsc;

use rand::Rng;

use oddity_rtsp_protocol as rtsp;
use oddity_video as video;

use crate::runtime::Runtime;
use crate::runtime::task_manager::{Task, TaskContext};
use crate::source::SourceDelegate;
use crate::session::setup::{SessionSetup, SessionSetupTarget};
use crate::media::video::rtp_muxer;

pub enum SessionState {
  Stopped(SessionId),
}

pub type SessionStateTx = mpsc::UnboundedSender<SessionState>;
pub type SessionStateRx = mpsc::UnboundedReceiver<SessionState>;

pub struct Session {
  worker: Task,
}

impl Session {

  pub async fn setup_and_start(
    id: SessionId,
    source_delegate: SourceDelegate,
    setup: SessionSetup,
    state_tx: SessionStateTx,
    runtime: &Runtime,
  ) -> Self {
    tracing::trace!(%id, "starting session");
    let worker = runtime
      .task()
      .spawn({
        let id = id.clone();
        |task_context| {
          Self::run(
            id,
            source_delegate,
            setup,
            state_tx,
            task_context,
          )
        }
      })
      .await;
    tracing::trace!(%id, "started session");

    Self {
      worker,
    }
  }

  pub async fn teardown(&mut self) {
    tracing::trace!("sending teardown signal to session");
    let _ = self.worker.stop().await;
    tracing::trace!("session torn down");
  }

  async fn run(
    id: SessionId,
    source_delegate: SourceDelegate,
    setup: SessionSetup,
    state_tx: SessionStateTx,
    task_context: TaskContext,
  ) {
    let mut muxer = setup.rtp_muxer;

    match setup.rtp_target {
      SessionSetupTarget::RtpUdp(target) => {
        tracing::trace!(%id, "starting rtp over udp loop");
        Self::run_udp(
          id.clone(),
          source_delegate,
          &mut muxer,
          target,
          task_context,
        ).await;
      },
      SessionSetupTarget::RtpTcp(target) => {
        tracing::trace!(%id, "starting rtp over tcp (interleaved) loop");
        Self::run_tcp(
          id.clone(),
          source_delegate,
          &mut muxer,
          target,
          task_context,
        ).await;
      },
    };

    tracing::trace!(%id, "finishing muxer");
    // Throw away possible last RTP buffer (we don't care about
    // it since this is real-time and there's no "trailer".
    let _ = rtp_muxer::finish(muxer).await;
    tracing::trace!(%id, "finished muxer");

    let _ = state_tx.send(SessionState::Stopped(id));
  }

  async fn run_udp(
    id: SessionId,
    mut source_delegate: SourceDelegate,
    muxer: &mut video::RtpMuxer,
    target: setup::SendOverSocket,
    mut task_context: TaskContext,
  ) {
    let socket_rtp = match net::UdpSocket::bind("0.0.0.0:0").await {
      Ok(socket) => socket,
      Err(err) => {
        tracing::error!(%id, %err, "failed to bind UDP socket");
        return;
      },
    };

    let socket_rtcp = match net::UdpSocket::bind("0.0.0.0:0").await {
      Ok(socket) => socket,
      Err(err) => {
        tracing::error!(%id, %err, "failed to bind UDP socket");
        return;
      },
    };

    loop {
      select! {
        packet = source_delegate.recv_packet() => {
          match packet {
            Some(packet) => {
              let packet = match muxer.mux(packet) {
                Ok(packet) => packet,
                Err(err) => {
                  tracing::error!(%id, %err, "failed to mux packet");
                  break;
                },
              };

              let sent = match packet {
                video::RtpBuf::Rtp(buf) => {
                  socket_rtp.send_to(&buf, target.rtp_remote).await
                },
                video::RtpBuf::Rtcp(buf) => {
                  socket_rtp.send_to(&buf, target.rtcp_remote).await
                }
              };

              if let Err(err) = sent {
                tracing::error!(%id, %err, "socket failed");
                break;
              }
            },
            None => {
              tracing::error!(%id, "source broken");
              break;
            },
          }
        },
        _ = task_context.wait_for_stop() => {
          tracing::trace!("tearing down session");
          break;
        },
      }
    }
  }

  async fn run_tcp(
    id: SessionId,
    mut source_delegate: SourceDelegate,
    muxer: &mut video::RtpMuxer,
    target: setup::SendInterleaved,
    mut task_context: TaskContext,
  ) {
    loop {
      select! {
        packet = source_delegate.recv_packet() => {
          match packet {
            Some(packet) => {
              let packet = match muxer.mux(packet) {
                Ok(packet) => packet,
                Err(err) => {
                  tracing::error!(%id, %err, "failed to mux packet");
                  break;
                },
              };

              let rtsp_interleaved_message = match packet {
                video::RtpBuf::Rtp(payload) => {
                  rtsp::ResponseMaybeInterleaved::Interleaved {
                    channel: target.rtp_channel,
                    payload: payload.into(),
                  }
                },
                video::RtpBuf::Rtcp(payload) => {
                  rtsp::ResponseMaybeInterleaved::Interleaved {
                    channel: target.rtcp_channel,
                    payload: payload.into(),
                  }
                },
              };

              if let Err(err) = target.sender.send(rtsp_interleaved_message) {
                tracing::trace!(%id, %err, "underlying connection closed");
                break;
              }
            }
            None => {
              tracing::error!(%id, "source broken");
              break;
            },
          }
        },
        _ = task_context.wait_for_stop() => {
          tracing::trace!("tearing down session");
          break;
        },
      }
    }
  }

}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SessionId(String);

impl SessionId {
  const SESSION_ID_LEN: usize = 16;

  pub fn generate() -> SessionId {
    SessionId(
      rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(Self::SESSION_ID_LEN)
        .map(char::from)
        .collect()
    )
  }

}

impl fmt::Display for SessionId {

  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    self.0.fmt(f)
  }

}