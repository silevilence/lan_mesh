use crate::{Message, message_to_json};
use std::{fmt, io};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub enum FrameError {
    Io(io::Error),
    Json(serde_json::Error),
    FrameTooLarge { len: usize, max: usize },
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::Json(err) => write!(f, "JSON error: {err}"),
            Self::FrameTooLarge { len, max } => {
                write!(f, "frame is too large: {len} bytes exceeds {max}")
            }
        }
    }
}

impl std::error::Error for FrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Json(err) => Some(err),
            Self::FrameTooLarge { .. } => None,
        }
    }
}

impl From<io::Error> for FrameError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<serde_json::Error> for FrameError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}

pub async fn write_message_frame<W>(writer: &mut W, message: &Message) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
{
    let body = message_to_json(message)?.into_bytes();
    if body.len() > MAX_FRAME_LEN {
        return Err(FrameError::FrameTooLarge {
            len: body.len(),
            max: MAX_FRAME_LEN,
        });
    }

    let len = u32::try_from(body.len()).map_err(|_| FrameError::FrameTooLarge {
        len: body.len(),
        max: u32::MAX as usize,
    })?;

    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&body).await?;
    Ok(())
}

pub async fn read_message_frame<R>(reader: &mut R) -> Result<Message, FrameError>
where
    R: AsyncRead + Unpin,
{
    let mut prefix = [0; 4];
    reader.read_exact(&mut prefix).await?;
    let len = u32::from_be_bytes(prefix) as usize;
    if len > MAX_FRAME_LEN {
        return Err(FrameError::FrameTooLarge {
            len,
            max: MAX_FRAME_LEN,
        });
    }

    let mut body = vec![0; len];
    reader.read_exact(&mut body).await?;
    Ok(serde_json::from_slice(&body)?)
}
