# Rustty

Terminal para Linux, escrito em Rust do zero. O render é Vulkan, via wgpu, e o scrollback que sai da tela é indexado num buffer residente na NPU Intel (`intel_vpu`).

O clone do [Kitty](https://github.com/kovidgoyal/kitty) em `../kitty` serviu só para ler as metas públicas de desempenho (glifo em VRAM, parser fora do frame, pouco tráfego até a GPU). O código do Kitty é GPL e não entra neste repositório.

## O que está no caminho quente

- PTY numa thread, parser `vte` noutra, janela só acorda para desenhar.
- Grade de células de 16 bytes. Só as linhas sujas sobem para a GPU.
- Um draw instanciado: o vertex shader lê a célula, busca o retângulo no atlas e emite o quad. O fragmento mistura cobertura do glifo com frente e fundo.
- Atlas R8 em VRAM. Espaço e células de continuação de caractere largo não rasterizam.
- Hash da linha em AVX2, com o mesmo resultado do caminho escalar, gravado num anel de 1 MiB mapeado no endereço virtual da NPU.

## NPU

O firmware da NPU só executa command buffers compilados pela Intel. O Rustty não inventa um kernel. O que ele faz no hardware:

1. Abre `/dev/accel/accel*`, confere o driver `intel_vpu` e lê device id, clock, contexto e firmware.
2. Cria um GEM buffer mapeável e uma command queue em modo turbo, prioridade focus.
3. Escreve o anel de scrollback nesse buffer (`vpu_addr` acima de 4 GiB, snooped).

Um grafo compilado entra por `RUSTTY_NPU_GRAPH`. O arquivo começa com o magic `RTY1`, um `u32` de flags, um `u32` com o tamanho do command buffer e 4 bytes reservados. Os bytes seguintes são o command buffer. O submit manda esse buffer, o anel e o buffer de preempção que o driver pede.

Sem esse arquivo a NPU fica com o anel residente e o cálculo do hash continua na CPU, na thread `rustty-npu`.

## Uso

```bash
cargo run --release
cargo run --release --bin rustty-probe
cargo run --release --bin rustty-bench
```

`rustty-probe` imprime a NPU e o adapter Vulkan. `rustty-bench` mede o parser em texto ASCII, Unicode e SGR.

Variáveis:

| Variável | Efeito |
| --- | --- |
| `RUSTTY_FONT` | caminho da fonte regular |
| `RUSTTY_FONT_BOLD` | caminho da fonte bold |
| `RUSTTY_FONT_SIZE` | tamanho em px lógicos, padrão 15 |
| `RUSTTY_VSYNC` | `0` desliga o vsync (`AutoNoVsync`) |
| `RUSTTY_NPU_GRAPH` | command buffer `RTY1` para submeter na NPU |
| `RUST_LOG` | filtro do tracing, padrão `info` |

Atalhos locais: Shift+PageUp e Shift+PageDown rolam o scrollback. O resto vai para o shell (`$SHELL`, `TERM=xterm-256color`).

## Fora deste corte

Abas, protocolo gráfico, ligaduras, IME e clipboard. O parser cobre o que um shell e um editor usam no dia a dia: cursor, SGR com cor de 256 e truecolor, região de scroll, tela alternativa, modo de cursor de aplicação, update sincronizado (2026) e reporte de cursor.
