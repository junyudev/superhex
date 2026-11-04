//! Explicit live verification. Prints only timings, counts, and failure codes.
//! cargo run --manifest-path crates/voice-buddy/Cargo.toml --features live --example live -- --config /private/context.json --wav /private/clip.wav --pace 1
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use voice_buddy::{Config, Session};
fn main() {
    if run().is_err() {
        eprintln!("live verification failed");
        std::process::exit(1);
    }
}
fn run() -> Result<(), ()> {
    let args: Vec<String> = std::env::args().collect();
    let value = |flag: &str| {
        args.windows(2)
            .find(|v| v[0] == flag)
            .map(|v| v[1].as_str())
    };
    let config = Arc::new(
        Config::parse(&std::fs::read(value("--config").ok_or(())?).map_err(|_| ())?)
            .map_err(|_| ())?,
    );
    let started = Instant::now();
    if args.iter().any(|a| a == "--check") {
        voice_buddy::check(&config).map_err(|e| {
            eprintln!("{e}");
        })?;
        println!("check_ms={}", started.elapsed().as_millis());
        return Ok(());
    }
    let mut wav = hound::WavReader::open(value("--wav").ok_or(())?).map_err(|_| ())?;
    let spec = wav.spec();
    if spec.sample_rate != 16_000 || spec.channels != 1 || spec.bits_per_sample != 16 {
        return Err(());
    }
    let samples: Vec<f32> = wav
        .samples::<i16>()
        .map(|n| n.map(|n| f32::from(n) / 32768.0))
        .collect::<Result<_, _>>()
        .map_err(|_| ())?;
    let mut options = if args.iter().any(|a| a == "--defaults") {
        voice_buddy::Options::default()
    } else {
        config.options()
    };
    if args.iter().any(|a| a == "--no-polish") {
        options.polish = false;
    }
    let pace: f64 = value("--pace").unwrap_or("1").parse().map_err(|_| ())?;
    if !pace.is_finite() || pace < 0.0 {
        return Err(());
    }
    let mut partials = 0;
    let mut first_ms = None;
    let mut polishing = false;
    let mut observe = |update: &voice_buddy::Update| {
        if !update.text.is_empty() {
            partials += 1;
            first_ms.get_or_insert(started.elapsed().as_millis());
        }
        polishing |= update.polishing;
    };
    let release;
    let result = if args.iter().any(|a| a == "--transcribe") {
        release = Instant::now();
        voice_buddy::transcribe(config, options, &samples, None, &mut observe)
    } else {
        let mut session = Session::start(config, options);
        for (index, chunk) in samples.chunks(320).enumerate() {
            session.feed(chunk);
            if let Some(update) = session.take_update() {
                observe(&update);
            }
            if pace > 0.0 {
                std::thread::sleep(
                    (started + Duration::from_secs_f64((index + 1) as f64 * 0.02 / pace))
                        .saturating_duration_since(Instant::now()),
                );
            }
        }
        release = Instant::now();
        let pending = session.finish();
        let cancel = pending.canceller();
        if args.iter().any(|a| a == "--cancel") {
            cancel.cancel();
        }
        pending.wait(&mut observe)
    };
    println!(
        "audio_ms={} first_partial_ms={} partial_updates={} polishing={} release_to_final_ms={} total_ms={}",
        samples.len() * 1000 / 16_000,
        first_ms
            .map(|n| n.to_string())
            .unwrap_or_else(|| "none".into()),
        partials,
        polishing,
        release.elapsed().as_millis(),
        started.elapsed().as_millis()
    );
    match result {
        Ok(text) => {
            println!("final_chars={}", text.chars().count());
            if let Some(path) = value("--output") {
                use std::io::Write;
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut file = options.open(path).map_err(|_| ())?;
                file.write_all(text.as_bytes()).map_err(|_| ())?;
            }
            Ok(())
        }
        Err(error) => {
            println!("failure={}", error.failure);
            if args.iter().any(|a| a == "--cancel")
                && error.failure == voice_buddy::Failure::Internal("cancelled".into())
            {
                return Ok(());
            }
            Err(())
        }
    }
}
