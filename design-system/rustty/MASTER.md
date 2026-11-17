# Identidade visual — Rustty

## Ideia

Rustty é um terminal Linux nativo para quem passa o dia no shell. O projeto combina uma grade de células renderizada com Vulkan, sessões independentes em abas e indexação do scrollback. A identidade deve transmitir precisão, agilidade e autonomia sem disputar atenção com o conteúdo do terminal.

O sinal `>` representa o prompt; o bloco representa o cursor pronto para o próximo comando. O cobre remete ao nome Rustty e funciona como um ponto de orientação, não como cor de fundo para o texto. A caixa escura, de cantos arredondados e construção geométrica, aproxima o ícone de uma janela de terminal.

**Expressão:** direto, técnico, calmo e acolhedor. Frase de apoio sugerida: “Seu fluxo, sem ruído.” Evitar metáforas de IA ou promessas de aceleração de NPU: a execução de um grafo depende de um command buffer compilado e configurado.

## Marca

- [Ícone vetorial](../../assets/brand/rustty-icon.svg): aplicação, lançadores e perfis. Preserve a proporção e a margem transparente original.
- [Ícone PNG 512 px](../../assets/brand/rustty-icon.png): usos que não aceitam SVG.
- [Símbolo sem caixa](../../assets/brand/rustty-mark.svg): composições sobre fundos escuros, sempre com contraste suficiente.
- [Assinatura para fundo escuro](../../assets/brand/rustty-lockup.svg) e [para fundo claro](../../assets/brand/rustty-lockup-light.svg): ícone e nome juntos, sem alterar o espaçamento ou a proporção.
- [Nome em vetor](../../assets/brand/rustty-wordmark.svg): desenho baseado em Red Hat Display Bold, convertido em curvas para não depender da fonte instalada. O nome de produto em texto corrido continua sendo **Rustty**.

Em tamanhos pequenos, prefira o ícone sozinho. Não estique, rotacione, adicione contornos ou coloque o símbolo sobre fundos de baixo contraste. O ícone e o nome são as fontes para os demais arquivos: depois de editá-los, execute `scripts/render-brand-assets.sh` para regenerar o símbolo, as assinaturas, o PNG e o RGBA incorporado à janela. Ao alterar o ícone do Rustty, execute também `scripts/render-agent-icons.sh` para atualizar o ícone das abas. Os scripts requerem Python 3 e ImageMagick com suporte a SVG; o atlas das abas também usa Pillow.

## Paleta do tema Rustty

Os valores são implementados em `src/color.rs`; outros temas continuam disponíveis e são identificados por seus próprios IDs.

| Papel | Cor | Uso |
| --- | --- | --- |
| Fundo | `#131619` | Área de trabalho e aba ativa |
| Texto | `#EDEAE4` | Conteúdo padrão e textos principais |
| Faixa | `#1B1E22` | Fundo da barra de abas |
| Superfície | `#252A2E` | Abas inativas e painel de aparência |
| Texto secundário | `#B6B8BB` | Títulos inativos e legendas |
| Cobre | `#F0A77D` | Número da aba ativa, seleção e controles em evidência |

Pares principais medidos em contraste WCAG: texto/fundo **15,12:1**; cobre/superfície **7,26:1**; secundário/superfície **7,29:1**. A paleta ANSI continua independente, para não mudar as cores explicitamente pedidas pelos programas. Use o cobre com parcimônia e preserve o contraste dos textos ao criar novos componentes.

## Tipografia e interface

A assinatura usa Red Hat Display Bold em curvas. Na grade e nos controles do aplicativo, mantenha a fonte monoespaçada escolhida pela pessoa; JetBrains Mono é uma boa referência visual, mas não é uma dependência nem substitui uma preferência salva. Os tamanhos da grade seguem a configuração existente.

A barra é compacta e fica no rodapé, deixando espaço para os comandos. A aba ativa se conecta visualmente ao fundo do terminal e seu número recebe o cobre; o painel usa uma superfície um pouco mais clara. Os ícones dos agentes ocupam a mesma caixa quadrada, preservando a proporção de cada marca. A hierarquia também se apoia em posição, texto e fundo, sem depender apenas da cor. Usar espaçamento em múltiplos de células na interface nativa. Evitar efeitos luminosos, texturas e animações que atrapalhem a leitura ou tornem a resposta do terminal mais lenta.

A escolha do tema continua no painel de aparência (`Ctrl+,`) e em `ui.conf`. A identidade Rustty é o padrão quando ainda não há preferência; uma escolha anterior como `noite` ou `papel` permanece válida.
