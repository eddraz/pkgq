//! Query preprocessing: accent folding, stopword removal and ES→EN synonym
//! expansion. Package descriptions are mostly English, so Spanish tokens are
//! translated before scoring; localized metadata (e.g. flatpak) still matches
//! through the original tokens.

use std::collections::HashMap;
use std::sync::OnceLock;

/// Common Spanish/English stopwords that only add noise as substring tokens.
const STOPWORDS: &[&str] = &[
    "de", "la", "el", "los", "las", "para", "con", "por", "un", "una", "del", "al", "lo", "les",
    "the", "an", "of", "to", "for", "and", "or", "with", "in", "on", "at", "is",
];

/// Spanish → English technical synonyms. Values must be a single token.
fn synonyms() -> &'static HashMap<&'static str, &'static str> {
    static SYNONYMS: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    SYNONYMS.get_or_init(|| {
        HashMap::from([
            ("compresor", "compressor"),
            ("descompresor", "decompressor"),
            ("comprimir", "compress"),
            ("comprimido", "compressed"),
            ("descomprimir", "decompress"),
            ("reproductor", "player"),
            ("reproducir", "play"),
            ("navegador", "browser"),
            ("navegacion", "browsing"),
            ("buscador", "finder"),
            ("gestor", "manager"),
            ("administrador", "manager"),
            ("archivo", "file"),
            ("archivos", "files"),
            ("carpeta", "folder"),
            ("disco", "disk"),
            ("imagen", "image"),
            ("imagenes", "images"),
            ("sonido", "sound"),
            ("musica", "music"),
            ("juego", "game"),
            ("juegos", "games"),
            ("videojuego", "game"),
            ("red", "network"),
            ("correo", "mail"),
            ("mensajeria", "messaging"),
            ("mensaje", "message"),
            ("descarga", "download"),
            ("descargar", "download"),
            ("descargador", "downloader"),
            ("subir", "upload"),
            ("grabadora", "burner"),
            ("grabar", "record"),
            ("pantalla", "screen"),
            ("captura", "screenshot"),
            ("foto", "photo"),
            ("fotos", "photos"),
            ("dibujo", "drawing"),
            ("dibujar", "draw"),
            ("pintar", "paint"),
            ("calculadora", "calculator"),
            ("calculo", "calculation"),
            ("planilla", "spreadsheet"),
            ("hoja", "sheet"),
            ("texto", "text"),
            ("teclado", "keyboard"),
            ("impresora", "printer"),
            ("imprimir", "print"),
            ("cifrado", "encryption"),
            ("encriptacion", "encryption"),
            ("contrasena", "password"),
            ("clave", "key"),
            ("seguridad", "security"),
            ("privacidad", "privacy"),
            ("memoria", "memory"),
            ("camara", "camera"),
            ("microfono", "microphone"),
            ("altavoz", "speaker"),
            ("mapa", "map"),
            ("clima", "weather"),
            ("calendario", "calendar"),
            ("reloj", "clock"),
            ("nota", "note"),
            ("notas", "notes"),
            ("utilidad", "utility"),
            ("herramienta", "tool"),
            ("herramientas", "tools"),
            ("servidor", "server"),
            ("cliente", "client"),
            ("datos", "data"),
            ("instalar", "install"),
            ("instalacion", "install"),
            ("actualizar", "update"),
            ("actualizacion", "update"),
        ])
    })
}

/// Lowercase and strip the most common Latin-1 diacritics (`vídeo` →
/// `video`, `cálculo` → `calculo`) without pulling in a unicode crate.
pub(crate) fn fold_accents(token: &str) -> String {
    // Lowercase first so uppercase accented characters also fold.
    let token = token.to_lowercase();
    let mut out = String::with_capacity(token.len());
    for c in token.chars() {
        out.push(match c {
            'á' | 'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            other => other,
        });
    }
    out.to_lowercase()
}

/// Normalize a query into scored tokens: accent-folded, lowercased, without
/// stopwords, and with Spanish synonyms expanded to their English counterpart
/// (the original token is kept too, so localized metadata still matches).
pub(crate) fn expand_query(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in query.split_whitespace() {
        let token = fold_accents(raw);
        if token.len() < 2 && token != "c" && token != "r" {
            continue; // single chars other than c/r are noise
        }
        if STOPWORDS.contains(&token.as_str()) {
            continue;
        }
        if !out.contains(&token) {
            out.push(token.clone());
        }
        if let Some(english) = synonyms().get(token.as_str()) {
            let english = english.to_string();
            if !out.contains(&english) {
                out.push(english);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_accents_and_lowercases() {
        assert_eq!(fold_accents("VÍDEO"), "video");
        assert_eq!(fold_accents("Cálculo"), "calculo");
        assert_eq!(fold_accents("Ñandú"), "nandu");
    }

    #[test]
    fn expands_synonyms_and_drops_stopwords() {
        assert_eq!(expand_query("editor de video"), vec!["editor", "video"]);
        assert_eq!(
            expand_query("compresor de archivos"),
            vec!["compresor", "compressor", "archivos", "files"]
        );
        assert_eq!(
            expand_query("reproductor de música"),
            vec!["reproductor", "player", "musica", "music"]
        );
    }

    #[test]
    fn stopwords_alone_yield_no_tokens() {
        assert!(expand_query("de la el para").is_empty());
    }
}
