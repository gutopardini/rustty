# Ícones das abas

Os arquivos em `source/` são usados somente para identificar o CLI em execução na aba. As marcas pertencem aos respectivos titulares.

| Agente | Origem |
| --- | --- |
| Claude | [Ícone do aplicativo Claude](https://claude.ai/images/claude_app_icon.png) |
| Codex | Ícone distribuído no plugin oficial OpenAI `cloud-environment`, no arquivo `assets/codex.svg` |
| Kiro | [Ícone do site Kiro](https://kiro.dev/icon.svg) |
| Antigravity | [Ícone do site Google Antigravity](https://antigravity.google/favicon.svg) |
| Cursor | [Ícone do site Cursor](https://cursor.com/marketing-static/favicon.svg) |

O atlas mantém a ordem Claude, Codex, Kiro, Antigravity, Cursor, a engrenagem de configurações e o ícone do Rustty. Cada imagem fica numa caixa transparente de 64 × 64, sem distorcer a proporção original. Para recriá-lo, execute `scripts/render-agent-icons.sh`; são necessários Python 3, Pillow e ImageMagick com suporte a SVG. O aplicativo incorpora o atlas e não depende desses programas em execução.
