//! Traduz erros técnicos em mensagens simples, com o que a pessoa pode fazer.

fn has(text: &str, any: &[&str]) -> bool {
    any.iter().any(|p| text.contains(p))
}

/// Mensagem para quando o projeto não pôde ser concluído.
pub fn error(raw: &str) -> String {
    let t = raw.to_lowercase();
    if t.contains("cancelado") {
        return "Você cancelou este projeto.".into();
    }
    if has(
        &t,
        &[
            "sign in to confirm",
            "login required",
            "use --cookies",
            "confirm your age",
            "age-restricted",
        ],
    ) {
        return "O site pediu login para liberar este vídeo. Abra o vídeo no Firefox ou no Chrome com a sua conta \
conectada, feche o navegador e clique em \"Tentar de novo\"."
            .into();
    }
    if has(&t, &["private video", "video is private", "vídeo privado"]) {
        return "Este vídeo é privado. Só é possível usar vídeos públicos ou que você possa assistir sem login.".into();
    }
    if has(
        &t,
        &[
            "video unavailable",
            "has been removed",
            "http error 404",
            "not available in your country",
            "geo restrict",
        ],
    ) {
        return "Este vídeo não está disponível (foi removido ou está bloqueado na sua região)."
            .into();
    }
    if has(
        &t,
        &[
            "unsupported url",
            "is not a valid url",
            "no video formats found",
            "no media found",
        ],
    ) {
        return "Não encontramos um vídeo neste link. Confira se o link abre o vídeo diretamente e tente de novo.".into();
    }
    if has(&t, &["live event", "is live", "premieres in"]) {
        return "Este vídeo é uma transmissão ao vivo ou ainda não estreou. Tente de novo quando ele terminar.".into();
    }
    if has(&t, &["no space left", "espaço insuficiente", "disk full"]) {
        return "O disco do computador está cheio. Libere espaço e tente de novo.".into();
    }
    if has(
        &t,
        &[
            "invalid api key",
            "invalid_api_key",
            "incorrect api key",
            "401 unauthorized",
            "respondeu 401",
        ],
    ) {
        return "A chave do serviço online não foi aceita. Abra ⚙ Ajustes e cole a chave de novo."
            .into();
    }
    if has(&t, &["respondeu 429", "rate limit", "rate_limit", "quota"]) {
        return "O serviço online gratuito atingiu o limite de uso por agora. Espere alguns minutos e tente de novo."
            .into();
    }
    if has(
        &t,
        &[
            "error sending request",
            "connection refused",
            "connection reset",
            "timed out",
            "temporary failure in name resolution",
            "failed to resolve",
            "network is unreachable",
            "dns error",
            "falha ao conectar",
            "unable to download webpage",
        ],
    ) {
        return "Não foi possível conectar à internet ou o site não respondeu. Verifique sua conexão e tente de novo."
            .into();
    }
    if has(
        &t,
        &[
            "falha ao baixar ggml",
            "falha ao baixar qwen",
            "falha ao baixar llama",
            "download incompleto",
            "download falhou",
        ],
    ) {
        return "Não foi possível baixar os arquivos de inteligência artificial. Verifique sua internet e tente de novo."
            .into();
    }
    if has(&t, &["nenhuma fala", "no speech"]) {
        return "Não encontramos fala neste vídeo. O ClipForge funciona com vídeos em que pessoas falam (podcasts, \
aulas, entrevistas, vlogs)."
            .into();
    }
    if has(&t, &["muito curto"]) {
        return "Este vídeo é curto demais para gerar clipes.".into();
    }
    if has(
        &t,
        &[
            "não foi possível ler o vídeo",
            "invalid data found",
            "moov atom not found",
        ],
    ) {
        return "Não conseguimos abrir este arquivo de vídeo. Ele pode estar corrompido ou incompleto.".into();
    }
    if has(&t, &["yt-dlp", "não foi possível baixar o vídeo"]) {
        return "Não conseguimos baixar este vídeo. Tente de novo em alguns minutos; se continuar, baixe o vídeo \
e envie o arquivo pela aba \"Arquivo\"."
            .into();
    }
    if has(&t, &["whisper", "transcrever", "extrair o áudio"]) {
        return "Não conseguimos transformar a fala deste vídeo em texto. Tente de novo; se continuar, feche outros \
programas para liberar memória."
            .into();
    }
    if has(&t, &["renderizar"]) {
        return "Não conseguimos gerar o vídeo do clipe. Tente de novo; se continuar, verifique se há espaço no disco."
            .into();
    }
    "Algo deu errado. Clique em \"Tentar de novo\". Se continuar, veja os detalhes técnicos abaixo."
        .into()
}

/// Aviso para quando a IA não pôde ser usada e os cortes foram escolhidos automaticamente.
pub fn ai_notice(raw: &str) -> String {
    let t = raw.to_lowercase();
    let why = if has(
        &t,
        &[
            "memória",
            "encerrou",
            "out of memory",
            "failed to load model",
            "demorou demais",
        ],
    ) {
        "a inteligência artificial precisa de mais memória do que o computador tem livre agora \
(feche outros programas ou escolha o nível \"Leve\" em ⚙ Ajustes)"
    } else if has(
        &t,
        &[
            "401",
            "invalid api key",
            "invalid_api_key",
            "incorrect api key",
        ],
    ) {
        "a chave do serviço online não foi aceita (confira em ⚙ Ajustes)"
    } else if has(&t, &["429", "rate limit", "quota"]) {
        "o serviço online gratuito atingiu o limite de uso por agora"
    } else if has(
        &t,
        &[
            "error sending request",
            "falha ao conectar",
            "timed out",
            "dns",
        ],
    ) {
        "não foi possível conectar ao serviço de inteligência artificial"
    } else if has(&t, &["baixar"]) {
        "não foi possível baixar a inteligência artificial"
    } else {
        "a inteligência artificial não respondeu como esperado"
    };
    format!("Os cortes foram escolhidos automaticamente porque {why}.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traduz_erros_comuns() {
        assert!(
            error("ERROR: [youtube] abc: Sign in to confirm you're not a bot").contains("login")
        );
        assert!(error("error sending request for url (https://x): dns error").contains("internet"));
        assert!(error("API de IA respondeu 401 Unauthorized: Invalid API key").contains("chave"));
        assert!(error("algo totalmente desconhecido").contains("Tentar de novo"));
        assert!(ai_notice("o llama-server encerrou (exit status: 137)").contains("memória"));
    }
}
