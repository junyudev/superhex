use crate::Failure;
use opus::{Application, Bandwidth, Bitrate, Channels, Encoder, Signal};

pub(crate) const FRAME: usize = 320;
pub(crate) const MAGIC: &[u8] = b"#!OPUS_RAW_V1\x02\x01\x00";
pub(crate) struct Audio {
    encoder: Encoder,
    tail: Vec<i16>,
    pub encoded: Vec<u8>,
    pub total: u64,
    pub samples: usize,
    pub sequence: u64,
}
impl Audio {
    pub fn new() -> Result<Self, Failure> {
        let mut encoder =
            Encoder::new(16_000, Channels::Mono, Application::Audio).map_err(error)?;
        encoder.set_bitrate(Bitrate::Bits(64_000)).map_err(error)?;
        encoder.set_complexity(10).map_err(error)?;
        encoder.set_signal(Signal::Voice).map_err(error)?;
        encoder.set_bandwidth(Bandwidth::Wideband).map_err(error)?;
        encoder.set_vbr(true).map_err(error)?;
        encoder.set_vbr_constraint(true).map_err(error)?;
        encoder.set_dtx(false).map_err(error)?;
        encoder.set_inband_fec(false).map_err(error)?;
        encoder.set_packet_loss_perc(0).map_err(error)?;
        encoder.set_lsb_depth(24).map_err(error)?;
        Ok(Self {
            encoder,
            tail: Vec::with_capacity(FRAME),
            encoded: Vec::new(),
            total: 0,
            samples: 0,
            sequence: 0,
        })
    }
    pub fn append(&mut self, mut samples: &[i16]) -> Result<(), Failure> {
        self.samples += samples.len();
        if !self.tail.is_empty() {
            let n = (FRAME - self.tail.len()).min(samples.len());
            self.tail.extend_from_slice(&samples[..n]);
            samples = &samples[n..];
            if self.tail.len() == FRAME {
                let frame: [i16; FRAME] = self.tail.as_slice().try_into().map_err(|_| error(()))?;
                self.encode(&frame)?;
                self.tail.clear();
            }
        }
        let (frames, remainder) = samples.as_chunks::<FRAME>();
        for frame in frames {
            self.encode(frame)?;
        }
        self.tail.extend_from_slice(remainder);
        Ok(())
    }
    fn encode(&mut self, frame: &[i16]) -> Result<(), Failure> {
        if self.total == 0 {
            self.encoded.extend_from_slice(MAGIC);
            self.total += MAGIC.len() as u64;
        }
        let mut packet = [0; 1500];
        let n = self.encoder.encode(frame, &mut packet).map_err(error)?;
        self.encoded.extend_from_slice(&(n as u16).to_le_bytes());
        self.encoded.extend_from_slice(&packet[..n]);
        self.total += (2 + n) as u64;
        Ok(())
    }
    pub fn finish(&mut self) -> Result<(), Failure> {
        if self.samples == 0 {
            return Err(Failure::Internal("empty audio".into()));
        }
        if self.tail.is_empty() {
            return Ok(());
        }
        self.tail.resize(FRAME, 0);
        let frame: [i16; FRAME] = self.tail.as_slice().try_into().map_err(|_| error(()))?;
        self.encode(&frame)?;
        self.tail.clear();
        Ok(())
    }
    pub fn take(&mut self) -> (u64, Vec<u8>) {
        self.sequence += 1;
        (self.sequence, std::mem::take(&mut self.encoded))
    }
}
fn error(_: impl Sized) -> Failure {
    Failure::Internal("Opus encoding failed".into())
}
