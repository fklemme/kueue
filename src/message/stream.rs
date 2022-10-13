use crate::message::error::{MessageError, ParseError};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub const READ_BUFFER_LEN: usize = 1024;

pub struct MessageStream {
    stream: TcpStream,
    read_buffer: Vec<u8>, // bufferes bytes read from stream, fixed size
    msg_buffer: Vec<u8>,  // holds msg chunks from previous reads, grows
}

impl MessageStream {
    pub fn new(stream: TcpStream) -> Self {
        MessageStream {
            stream,
            read_buffer: vec![0; READ_BUFFER_LEN],
            msg_buffer: Vec::new(),
        }
    }

    pub async fn send<T: Serialize>(&mut self, message: &T) -> Result<(), MessageError> {
        let buffer = serde_json::to_vec(message).unwrap();

        match self.stream.write_all(&buffer).await {
            Ok(()) => Ok(()),
            Err(e) => {
                eprintln!("Write error: {}", e); // for debugging
                Err(MessageError::SendFailed)
            }
        }
    }

    pub async fn receive<T: for<'a> Deserialize<'a>>(&mut self) -> Result<T, MessageError> {
        loop {
            // Parse message from message buffer
            match self.parse_message::<T>() {
                Ok(message) => return Ok(message),
                Err(ParseError::EofWhileParsing) => {} // okay, continue reading from stream
                Err(_) => return Err(MessageError::ReceiveFailed), // give up and propagate error
            }

            // Read more data from stream
            match self.stream.read(&mut self.read_buffer).await {
                Ok(bytes_read) if bytes_read == 0 => return Err(MessageError::ConnectionClosed),
                Ok(bytes_read) => {
                    // Move read bytes into message buffer and continue loop
                    self.msg_buffer.extend(&self.read_buffer[..bytes_read]);
                }
                Err(e) => {
                    eprintln!("Read error: {}", e); // for debugging
                    return Err(MessageError::ReceiveFailed);
                }
            }
        }
    }

    fn parse_message<T: for<'a> Deserialize<'a>>(&mut self) -> Result<T, ParseError> {
        // Try to parse T from msg_buffer
        let de = serde_json::Deserializer::from_slice(&self.msg_buffer);
        let mut message_iterator = de.into_iter::<T>();
        match message_iterator.next() {
            Some(result) => match result {
                Ok(message) => {
                    // Successfully read message. Remove consumed bytes from buffer.
                    let bytes_consumed = message_iterator.byte_offset();
                    self.msg_buffer.drain(..bytes_consumed);
                    return Ok(message);
                }
                Err(e) if e.is_eof() => {
                    // Incomplete message. We need to read more data from the stream.
                    Err(ParseError::EofWhileParsing)
                }
                Err(e) => {
                    // Bad things happend! We need to give up.
                    eprintln!("Parse error: {}", e); // for debugging
                    Err(ParseError::ParsingFailed)
                }
            },
            None => {
                // Happens when buffer is empty. We need to read data from stream.
                debug_assert!(self.msg_buffer.is_empty());
                Err(ParseError::EofWhileParsing)
            }
        }
    }
}
