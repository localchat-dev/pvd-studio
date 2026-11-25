use super::*;
use std::marker::PhantomData;

pub(super) type Wake = Arc<dyn Fn() + Send + Sync>;

pub(super) enum Listener {
    Direct(TcpListener),
    #[cfg(windows)]
    Shared(Box<crate::share::Endpoint>),
}

pub(super) fn bind_tcp(address: SocketAddr) -> io::Result<TcpListener> {
    let listener = TcpListener::bind(address)?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

impl Listener {
    pub(super) fn bind(
        address: SocketAddr,
        sharing: Option<ShareOptions>,
        wake: Wake,
    ) -> io::Result<Self> {
        #[cfg(windows)]
        if let Some(sharing) = sharing {
            return crate::share::Endpoint::bind(address, sharing, wake)
                .map(|endpoint| Self::Shared(Box::new(endpoint)));
        }
        #[cfg(not(windows))]
        let _ = (sharing, wake);
        bind_tcp(address).map(Self::Direct)
    }

    pub(super) fn address(&self) -> io::Result<SocketAddr> {
        match self {
            Self::Direct(listener) => listener.local_addr(),
            #[cfg(windows)]
            Self::Shared(endpoint) => Ok(endpoint.address()),
        }
    }

    pub(super) fn role(&self) -> Option<Arc<Mutex<ListenerRole>>> {
        match self {
            Self::Direct(_) => None,
            #[cfg(windows)]
            Self::Shared(endpoint) => Some(endpoint.role()),
        }
    }

    #[inline]
    pub(super) fn accept(&mut self, stopped: &dyn Fn() -> bool) -> io::Result<Incoming<'_>> {
        #[cfg(not(windows))]
        let _ = stopped;
        match self {
            Self::Direct(listener) => {
                let (stream, peer) = listener.accept()?;
                Ok(Incoming::direct(stream, peer))
            }
            #[cfg(windows)]
            Self::Shared(endpoint) => endpoint.accept(stopped),
        }
    }
}

pub(super) struct Incoming<'a> {
    pub(super) peer: SocketAddr,
    side: Input<'a>,
    #[cfg(windows)]
    relay: Option<crate::share::Broadcast<'a>>,
}

enum Input<'a> {
    Tcp(TcpStream, PhantomData<&'a ()>),
    #[cfg(windows)]
    Pipe(&'a mut crate::share::Follower),
}

impl<'a> Incoming<'a> {
    pub(super) fn direct(stream: TcpStream, peer: SocketAddr) -> Self {
        Self {
            peer,
            side: Input::Tcp(stream, PhantomData),
            #[cfg(windows)]
            relay: None,
        }
    }

    #[cfg(windows)]
    pub(super) fn following(follower: &'a mut crate::share::Follower, peer: SocketAddr) -> Self {
        Self {
            peer,
            side: Input::Pipe(follower),
            relay: None,
        }
    }

    #[cfg(windows)]
    pub(super) fn with_relay(mut self, relay: crate::share::Broadcast<'a>) -> Self {
        self.relay = Some(relay);
        self
    }

    fn tcp(&self) -> Option<&TcpStream> {
        match &self.side {
            Input::Tcp(stream, _) => Some(stream),
            #[cfg(windows)]
            Input::Pipe(_) => None,
        }
    }

    pub(super) fn configure(&self) -> io::Result<()> {
        self.tcp()
            .map(configure_receive_stream)
            .transpose()
            .map(|_| ())
    }

    pub(super) fn control_socket(&self) -> io::Result<Option<TcpStream>> {
        self.tcp().map(TcpStream::try_clone).transpose()
    }

    pub(super) fn shutdown(&self) {
        if let Some(stream) = self.tcp() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    #[inline]
    pub(super) fn read(
        &mut self,
        bytes: &mut [u8],
        stopped: &dyn Fn() -> bool,
    ) -> io::Result<usize> {
        #[cfg(not(windows))]
        let _ = stopped;
        match &mut self.side {
            Input::Tcp(stream, _) => stream.read(bytes),
            #[cfg(windows)]
            Input::Pipe(follower) => follower.read(bytes, stopped),
        }
    }

    #[inline]
    pub(super) fn share(&mut self, bytes: &[u8]) {
        #[cfg(not(windows))]
        let _ = bytes;
        #[cfg(windows)]
        if let Some(relay) = &mut self.relay {
            relay.data(bytes);
        }
    }

    pub(super) fn complete(&mut self) {
        #[cfg(windows)]
        if let Some(mut relay) = self.relay.take() {
            relay.complete();
        }
    }
}
