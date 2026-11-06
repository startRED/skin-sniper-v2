# AGENTS.md — Skin Sniper v2

Leia `docs/projeto.md` (o que construir) antes de qualquer coisa. Fatos da Steam: `docs/conhecimento.md`
e `docs/pesquisa-web.md`. Cada fato mora em um arquivo só; corrija na fonte, não duplique.
Fale português (Brasil) com o Erick. Código em inglês, comentários e docs em português.

O projeto antigo em Go (`E:\Documentos\ProjetosIA\Skin Sniper\skin-sniper`, `original`, `robo-ao-vivo`)
é só consulta. Nunca altere, pare ou reinicie nada lá sem pedido explícito.

## Comandos (a prova de pronto)

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cd web && npm ci && npm run check && npm run build    # quando web/package.json existir
cargo build --release                                  # gera target/release/skin-sniper.exe
```

## Regras

1. Nunca commitar chave, cookie, token, `data/` ou config local. Nunca imprimir segredo em log.
2. Todo parser de resposta externa tem fixture real em `testdata/` e teste que roda sem rede.
3. Não invente número de mercado. Sem medida, diga que não mediu; resposta da Steam não validada é hipótese.
4. Dinheiro em centavos inteiros (`i64`). Nada de `f64` para preço final nem ÷ 1,15.
5. Não adicione crate fora do `Cargo.toml` sem motivo escrito no PR.
6. Entrega: worktree próprio em `E:\Documentos\ProjetosIA\SkinSniperV2\worktrees\v2-<tarefa>`, ramo próprio,
   commit com arquivos pelo nome (nunca `git add -A`), push, PR para `main`, CI verde.
7. Tráfego real contra a Steam só com ritmo ≥ 3 s entre consultas e nunca em rajada: o IP é o mesmo do
   robô antigo, que continua ligado.
