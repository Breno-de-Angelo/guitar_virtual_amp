//! Batch transcription into the local tab repository (offline):
//!
//! ```text
//! cargo run --release -p guitar_desktop --bin transcribe -- [options] <audio files or folders>...
//! ```
//!
//! Options:
//!   --repo DIR        repository folder (default: the app's tab repository)
//!   --no-separation   don't separate the instruments even if a separator is
//!                     installed (BS-RoFormer, scripts/install_bs_roformer.sh,
//!                     is preferred over Demucs; GVA_SEPARATOR=demucs|bs-roformer
//!                     forces one)
//!   --guitar-model M  gaps (default) or electric: the local electric fine-tune
//!                     (guitar_electric_crnn.onnx in the models folder)
//!   --fingering F     learned (default when fingering_tagger.onnx is in the
//!                     models folder) or viterbi
//!   --tone            also estimate each guitar track's tone (experimental):
//!                     saved as a user preset named after the song and track,
//!                     with a matching IR in <data dir>/guitar_virtual_amp/tone_match/
//!                     (needs a separator; adds ~30 s per guitar track)
//!   --no-bass / --no-guitar
//!   --force           redo files already in the repository (same audio hash)
//!   --artist A --title T   only with a single file/video; otherwise taken from
//!                     "Artist - Title" file names (or the parent folder as artist)
//!   --youtube Q       search YouTube for Q (or use Q as a video link) and
//!                     transcribe the best match; repeatable. Needs yt-dlp
//!                     (scripts/install_ytdlp.sh). Only download what you have
//!                     the right to: see YouTube's Terms of Service.
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use guitar_core::backend::audio_file::AUDIO_EXTENSIONS;
use guitar_core::shared::tab::repository::{
    default_repository_dir, file_sha256, find_by_audio_hash, find_by_youtube_id,
};
use guitar_core::transcribe::fingering_net::{self, Fingering};
use guitar_core::transcribe::guitar_crnn::{self, GuitarModel};
use guitar_core::transcribe::{self, Request, separation, youtube};

fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

fn collect(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(path)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .collect();
        entries.sort();
        for entry in entries {
            collect(&entry, out);
        }
    } else if is_audio(path) {
        out.push(path.to_path_buf());
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut inputs = Vec::new();
    let (mut repo, mut separate, mut guitar, mut bass, mut force) = (None, true, true, true, false);
    let (mut artist, mut title) = (None, None);
    let mut youtube_queries: Vec<String> = Vec::new();
    let mut guitar_model = GuitarModel::Gaps;
    let mut fingering = Fingering::Learned;
    let mut tone = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--repo" => repo = args.next().map(PathBuf::from),
            "--no-separation" => separate = false,
            "--no-bass" => bass = false,
            "--no-guitar" => guitar = false,
            "--force" => force = true,
            "--tone" => tone = true,
            "--artist" => artist = args.next(),
            "--title" => title = args.next(),
            "--youtube" => youtube_queries.extend(args.next()),
            "--guitar-model" => match args.next().as_deref() {
                Some("gaps") => guitar_model = GuitarModel::Gaps,
                Some("electric") => guitar_model = GuitarModel::Electric,
                other => {
                    eprintln!("--guitar-model: use gaps ou electric (não {other:?})");
                    std::process::exit(2);
                }
            },
            "--fingering" => match args.next().as_deref() {
                Some("learned") => fingering = Fingering::Learned,
                Some("viterbi") => fingering = Fingering::Viterbi,
                other => {
                    eprintln!("--fingering: use learned ou viterbi (não {other:?})");
                    std::process::exit(2);
                }
            },
            "-h" | "--help" => {
                println!(
                    "{}",
                    include_str!("transcribe.rs")
                        .lines()
                        .take_while(|l| l.starts_with("//!"))
                        .map(|l| l.trim_start_matches("//!").trim_start_matches(' '))
                        .collect::<Vec<_>>()
                        .join("\n")
                );
                return;
            }
            _ => inputs.push(PathBuf::from(arg)),
        }
    }
    let Some(repo) = repo.or_else(default_repository_dir) else {
        eprintln!("sem pasta de repositório; use --repo");
        std::process::exit(2);
    };
    let mut files = Vec::new();
    for input in &inputs {
        collect(input, &mut files);
    }
    if files.is_empty() && youtube_queries.is_empty() {
        eprintln!(
            "nenhum arquivo de áudio ({}) encontrado. Use --help.",
            AUDIO_EXTENSIONS.join(", ")
        );
        std::process::exit(2);
    }
    if (artist.is_some() || title.is_some()) && files.len() + youtube_queries.len() > 1 {
        eprintln!("--artist/--title só valem para um arquivo");
        std::process::exit(2);
    }
    if guitar_model == GuitarModel::Electric && guitar_crnn::find_electric_model().is_none() {
        eprintln!(
            "--guitar-model electric: {} não está na pasta de modelos",
            guitar_crnn::ELECTRIC_MODEL_FILE
        );
        std::process::exit(2);
    }
    match (separate, separation::find_separator()) {
        (true, Some(separator)) => println!("Separação: {}", separator.name()),
        (true, None) => println!(
            "Separação: nenhum separador encontrado (rode scripts/install_bs_roformer.sh ou install_demucs.sh); transcrevendo da mixagem"
        ),
        (false, _) => println!("Separação: desligada"),
    }
    let models = transcribe::installed_models();
    let fingering_engine = match fingering {
        Fingering::Learned if models.fingering_tagger => fingering_net::ENGINE_NAME,
        _ => fingering_net::VITERBI_ENGINE_NAME,
    };
    println!(
        "Notas: {}; tempos: {}; dedilhado: {fingering_engine}",
        transcribe::guitar_engine_name(guitar_model),
        models.beat_tracker.unwrap_or("detector simples")
    );
    if !models.complete() {
        println!("  (rode scripts/install_transcription_models.sh para os modelos melhores)");
    }
    println!("Repositório: {}\n", repo.display());

    let (mut done, mut skipped, mut failed) = (0, 0, 0);
    for (i, file) in files.iter().enumerate() {
        let (mut file_artist, file_title) = transcribe::guess_artist_title(file);
        if file_artist.is_empty() {
            // "Artist/Album/Song.mp3" or "Artist/Song.mp3" layouts.
            let parents: Vec<String> = file
                .ancestors()
                .skip(1)
                .take(2)
                .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .collect();
            file_artist = parents.last().cloned().unwrap_or_default();
        }
        let request = Request {
            audio_path: file.clone(),
            title: title.clone().unwrap_or(file_title),
            artist: artist.clone().unwrap_or(file_artist),
            separate,
            guitar_model,
            fingering,
            tone,
            guitar,
            bass,
            output_dir: repo.clone(),
            spotify_track_id: None,
            youtube_video_id: None,
        };
        let label = format!(
            "[{}/{}] {} - {}",
            i + 1,
            files.len(),
            request.artist,
            request.title
        );
        if !force
            && let Ok(hash) = file_sha256(file)
            && let Some(existing) = find_by_audio_hash(&repo, &hash)
        {
            println!("{label}: já transcrito ({})", existing.display());
            skipped += 1;
            continue;
        }
        let started = std::time::Instant::now();
        let mut last_stage = None;
        let result = transcribe::run(
            &request,
            &mut |p| {
                if last_stage != Some(p.stage) {
                    last_stage = Some(p.stage);
                    println!("{label}: {} ({:.0}%)", p.stage.label(), p.overall * 100.0);
                }
            },
            &AtomicBool::new(false),
        );
        match result {
            Ok(output) => {
                let tracks: Vec<String> = output
                    .metadata
                    .tracks
                    .iter()
                    .map(|t| format!("{} {} notas ({})", t.name, t.notes, t.tuning_name))
                    .collect();
                for (track, preset) in &output.tone_presets {
                    println!("  timbre de \"{track}\": {}", preset.display());
                }
                println!(
                    "{label}: OK em {:.0}s, {:.0} bpm, {} -> {}\n",
                    started.elapsed().as_secs_f64(),
                    output.metadata.tempo_bpm,
                    tracks.join(", "),
                    output.tab_path.display()
                );
                done += 1;
            }
            Err(e) => {
                println!("{label}: ERRO {e}\n");
                failed += 1;
            }
        }
    }
    if !youtube_queries.is_empty() {
        let Some(ytdlp) = youtube::find_ytdlp() else {
            eprintln!("yt-dlp não encontrado: rode scripts/install_ytdlp.sh");
            std::process::exit(2);
        };
        for (i, query) in youtube_queries.iter().enumerate() {
            let label = format!("[YouTube {}/{}] {query}", i + 1, youtube_queries.len());
            let video = match youtube::parse_video_id(query) {
                Some(id) => youtube::video_info(&ytdlp, &id),
                None => youtube::search(&ytdlp, query, 10).and_then(|v| {
                    youtube::rank(v, query, "", None)
                        .into_iter()
                        .next()
                        .ok_or_else(|| "nada encontrado".to_string())
                }),
            };
            let video = match video {
                Ok(v) => v,
                Err(e) => {
                    println!("{label}: ERRO {e}\n");
                    failed += 1;
                    continue;
                }
            };
            println!(
                "{label}: {} ({}) {}",
                video.title,
                video.channel_name(),
                video.url()
            );
            if !force && let Some(existing) = find_by_youtube_id(&repo, &video.id) {
                println!("{label}: já transcrito ({})\n", existing.display());
                skipped += 1;
                continue;
            }
            let (video_artist, video_title) = video.guess_artist_title();
            let request = Request {
                audio_path: PathBuf::new(),
                title: title.clone().unwrap_or(video_title),
                artist: artist.clone().unwrap_or(video_artist),
                separate,
                guitar_model,
                fingering,
                tone,
                guitar,
                bass,
                output_dir: repo.clone(),
                spotify_track_id: None,
                youtube_video_id: None,
            };
            let started = std::time::Instant::now();
            let mut last_stage = None;
            let result = transcribe::run_youtube(
                &ytdlp,
                &video.id,
                request,
                &mut |p| {
                    if last_stage != Some(p.stage) {
                        last_stage = Some(p.stage);
                        println!("{label}: {} ({:.0}%)", p.stage.label(), p.overall * 100.0);
                    }
                },
                &AtomicBool::new(false),
            );
            match result {
                Ok(output) => {
                    for (track, preset) in &output.tone_presets {
                        println!("  timbre de \"{track}\": {}", preset.display());
                    }
                    println!(
                        "{label}: OK em {:.0}s, {:.0} bpm -> {}\n",
                        started.elapsed().as_secs_f64(),
                        output.metadata.tempo_bpm,
                        output.tab_path.display()
                    );
                    done += 1;
                }
                Err(e) => {
                    println!("{label}: ERRO {e}\n");
                    failed += 1;
                }
            }
        }
    }
    println!("Concluído: {done} transcritas, {skipped} já existiam, {failed} com erro.");
    if failed > 0 {
        std::process::exit(1);
    }
}
