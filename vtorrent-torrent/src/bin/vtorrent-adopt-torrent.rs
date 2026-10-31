use std::path::Path;
use tokio::io::AsyncReadExt;
use vtorrent_torrent::{
    engine_disk::{adopt_existing_torrent, migrate_partial_torrent},
    metainfo::Metainfo,
};

#[tokio::main]
async fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    let partial = args.get(1).is_some_and(|arg| arg == "--partial");
    if (!partial && args.len() != 3) || (partial && args.len() != 5) {
        eprintln!("Usage: vtorrent-adopt-torrent <metainfo.torrent> <download-directory>\n       vtorrent-adopt-torrent --partial <metainfo.torrent> <source-directory> <new-download-directory>");
        std::process::exit(2);
    }
    let result = async {
        let file = tokio::fs::File::open(&args[if partial { 2 } else { 1 }])
            .await
            .map_err(|error| error.to_string())?;
        let mut data = Vec::new();
        file.take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut data)
            .await
            .map_err(|error| error.to_string())?;
        if data.len() > 4 * 1024 * 1024 {
            return Err("metainfo exceeds 4 MiB".into());
        }
        let metainfo = Metainfo::from_bytes(&data).map_err(|error| error.to_string())?;
        if partial {
            let result =
                migrate_partial_torrent(&metainfo, Path::new(&args[3]), Path::new(&args[4]))
                    .await
                    .map_err(|error| error.to_string())?;
            println!(
                "Copied {} verified pieces; {} pieces need downloading",
                result.copied_pieces, result.missing_pieces
            );
        } else {
            adopt_existing_torrent(&metainfo, Path::new(&args[2]))
                .await
                .map_err(|error| error.to_string())?;
            println!(
                "Verified and adopted complete torrent {}",
                hex::encode(metainfo.info_hash)
            );
        }
        Ok::<(), String>(())
    }
    .await;
    if let Err(error) = result {
        eprintln!("Torrent adoption refused: {error}");
        std::process::exit(1);
    }
}
