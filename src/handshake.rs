use crate::compat::{AllowStd, HasContext};
use crate::WebSocketStream;
use log::*;
use pin_project::pin_project;
use std::future::Future;
use std::io::{Read, Write};
use std::pin::Pin;
use std::task::{Context, Poll};
use futures::io::{AsyncRead, AsyncWrite};
use tungstenite::handshake::client::Response;
use tungstenite::handshake::server::Callback;
use tungstenite::handshake::{HandshakeError as Error, HandshakeRole, MidHandshake as WsHandshake};
use tungstenite::{ClientHandshake, ServerHandshake, WebSocket};

pub(crate) async fn without_handshake<F, S>(stream: S, f: F) -> WebSocketStream<S>
where
    F: FnOnce(AllowStd<S>) -> WebSocket<AllowStd<S>> + Unpin,
    S: AsyncRead + AsyncWrite + Unpin,
{
    let start = SkippedHandshakeFuture(Some(SkippedHandshakeFutureInner { f, stream }));

    let ws = start.await;

    WebSocketStream::new(ws)
}

struct SkippedHandshakeFuture<F, S>(Option<SkippedHandshakeFutureInner<F, S>>);
struct SkippedHandshakeFutureInner<F, S> {
    f: F,
    stream: S,
}

impl<F, S> Future for SkippedHandshakeFuture<F, S>
where
    F: FnOnce(AllowStd<S>) -> WebSocket<AllowStd<S>> + Unpin,
    S: Unpin,
    AllowStd<S>: Read + Write,
{
    type Output = WebSocket<AllowStd<S>>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        let inner = self
            .get_mut()
            .0
            .take()
            .expect("future polled after completion");
        trace!("Setting context when skipping handshake");
        let stream = AllowStd {
            inner: inner.stream,
            context: (true, ctx as *mut _ as *mut ()),
        };

        Poll::Ready((inner.f)(stream))
    }
}

#[pin_project]
struct MidHandshake<Role: HandshakeRole>(Option<WsHandshake<Role>>);

enum StartedHandshake<Role: HandshakeRole> {
    Done(Role::FinalResult),
    Mid(WsHandshake<Role>),
}

struct StartedHandshakeFuture<F, S>(Option<StartedHandshakeFutureInner<F, S>>);
struct StartedHandshakeFutureInner<F, S> {
    f: F,
    stream: S,
}

async fn handshake<Role, F, S>(stream: S, f: F) -> Result<Role::FinalResult, Error<Role>>
where
    Role: HandshakeRole + Unpin,
    Role::InternalStream: HasContext,
    F: FnOnce(AllowStd<S>) -> Result<Role::FinalResult, Error<Role>> + Unpin,
    S: AsyncRead + AsyncWrite + Unpin,
{
    let start = StartedHandshakeFuture(Some(StartedHandshakeFutureInner { f, stream }));

    match start.await? {
        StartedHandshake::Done(r) => Ok(r),
        StartedHandshake::Mid(s) => {
            let res: Result<Role::FinalResult, Error<Role>> = MidHandshake::<Role>(Some(s)).await;
            res
        }
    }
}

pub(crate) async fn client_handshake<F, S>(
    stream: S,
    f: F,
) -> Result<(WebSocketStream<S>, Response), Error<ClientHandshake<AllowStd<S>>>>
where
    F: FnOnce(
            AllowStd<S>,
        ) -> Result<
            <ClientHandshake<AllowStd<S>> as HandshakeRole>::FinalResult,
            Error<ClientHandshake<AllowStd<S>>>,
        > + Unpin,
    S: AsyncRead + AsyncWrite + Unpin,
{
    let result = handshake(stream, f).await?;
    let (s, r) = result;
    Ok((WebSocketStream::new(s), r))
}

pub(crate) async fn server_handshake<C, F, S>(
    stream: S,
    f: F,
) -> Result<WebSocketStream<S>, Error<ServerHandshake<AllowStd<S>, C>>>
where
    C: Callback + Unpin,
    F: FnOnce(
            AllowStd<S>,
        ) -> Result<
            <ServerHandshake<AllowStd<S>, C> as HandshakeRole>::FinalResult,
            Error<ServerHandshake<AllowStd<S>, C>>,
        > + Unpin,
    S: AsyncRead + AsyncWrite + Unpin,
{
    let s: WebSocket<AllowStd<S>> = handshake(stream, f).await?;
    Ok(WebSocketStream::new(s))
}

impl<Role, F, S> Future for StartedHandshakeFuture<F, S>
where
    Role: HandshakeRole,
    Role::InternalStream: HasContext,
    F: FnOnce(AllowStd<S>) -> Result<Role::FinalResult, Error<Role>> + Unpin,
    S: Unpin,
    AllowStd<S>: Read + Write,
{
    type Output = Result<StartedHandshake<Role>, Error<Role>>;

    fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Self::Output> {
        let inner = self.0.take().expect("future polled after completion");
        trace!("Setting ctx when starting handshake");
        let stream = AllowStd {
            inner: inner.stream,
            context: (true, ctx as *mut _ as *mut ()),
        };

        match (inner.f)(stream) {
            Ok(r) => Poll::Ready(Ok(StartedHandshake::Done(r))),
            Err(Error::Interrupted(mut mid)) => {
                let machine = mid.get_mut();
                machine.get_mut().set_context((true, std::ptr::null_mut()));
                Poll::Ready(Ok(StartedHandshake::Mid(mid)))
            }
            Err(Error::Failure(e)) => Poll::Ready(Err(Error::Failure(e))),
        }
    }
}

impl<Role> Future for MidHandshake<Role>
where
    Role: HandshakeRole + Unpin,
    Role::InternalStream: HasContext,
{
    type Output = Result<Role::FinalResult, Error<Role>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let mut s = this.0.take().expect("future polled after completion");

        let machine = s.get_mut();
        trace!("Setting context in handshake");
        machine.get_mut().set_context((true, cx as *mut _ as *mut ()));

        match s.handshake() {
            Ok(stream) => Poll::Ready(Ok(stream)),
            Err(Error::Failure(e)) => Poll::Ready(Err(Error::Failure(e))),
            Err(Error::Interrupted(mut mid)) => {
                let machine = mid.get_mut();
                machine.get_mut().set_context((true, std::ptr::null_mut()));
                *this.0 = Some(mid);
                Poll::Pending
            }
        }
    }
}
