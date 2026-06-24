# battery-up

CLI para medir quanto tempo o notebook permaneceu rodando somente na bateria.
Ela pode medir uma sessão ativa no terminal ou rodar em segundo plano via
systemd e deixar a CLI apenas para consulta.

O projeto foi feito para Linux/NixOS e lê o estado diretamente de
`/sys/class/power_supply`.

## Uso

```sh
nix run .
```

O pacote padrão da flake é apenas o CLI, sem compilar a pilha COSMIC do applet.
Neste diretório local, se o Nix reclamar da pasta `.git` do ambiente, use:

```sh
nix run path:$PWD
```

Para imprimir apenas o estado atual:

```sh
nix run . -- --once
```

Saída em JSON:

```sh
nix run . -- --json
```

Intervalo de atualização:

```sh
nix run . -- --interval 5
```

## Uso com systemd

O daemon acumula o tempo em segundo plano:

```sh
nix run . -- daemon
```

Ao carregar, o daemon zera o contador e escreve um novo estado inicial. O arquivo
de estado anterior ainda pode ser lido por `status`, mas não é reutilizado como
ponto de partida de uma nova execução do daemon.

Depois consulte o total com:

```sh
nix run . -- status
```

O status separa o tempo ativo em bateria do tempo suspenso em bateria. Quando o
kernel expõe energia e potência da bateria, ele também mostra consumo médio e
previsão de autonomia baseada em `energy_now - energy_empty` dividido por uma
média móvel recente de `power_now`. Se esses campos não existirem, a previsão
cai para a estimativa antiga por queda percentual.

No JSON, os dados de standby aparecem como `standby_seconds`, `standby_hms`,
`standby_drop_percent` e `standby_drain_per_minute`. A previsão de autonomia
aparece como `power_now_watts`, `short_average_power_watts`,
`long_average_power_watts`, `energy_remaining_watt_hours`,
`estimated_remaining_seconds` e `estimated_remaining_hms`.

Para acompanhar o estado do daemon ao vivo no terminal:

```sh
nix run . -- status --live
```

Para zerar o acumulado:

```sh
nix run . -- reset
```

O arquivo de estado padrão é `/var/lib/battery-up/state`. Para testar sem root:

```sh
nix run . -- daemon --state-file /tmp/battery-up-state
nix run . -- status --state-file /tmp/battery-up-state
nix run . -- status --live --state-file /tmp/battery-up-state
```

### Módulo NixOS

Adicione a flake como input da sua configuração NixOS e importe o módulo:

```nix
{
  inputs.battery-up.url = "path:/home/lluz/tmp/battery_up";

  outputs = { nixpkgs, battery-up, ... }: {
    nixosConfigurations.SEUP_HOST = nixpkgs.lib.nixosSystem {
      modules = [
        battery-up.nixosModules.default
        {
          services.battery-up = {
            enable = true;
            interval = 1;
          };
        }
      ];
    };
  };
}
```

Após aplicar a configuração, consulte com:

```sh
nix run . -- status
```

### Uso por outra flake

Para consumir a versão publicada em outra flake:

```nix
{
  inputs.battery-up.url = "github:lluz55/battery_up/v0.1.4";

  outputs = { nixpkgs, battery-up, ... }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ battery-up.overlays.default ];
      };
    in
    {
      packages.${system}.default = pkgs.battery-up;
    };
}
```

Sem overlay, use diretamente:

```nix
battery-up.packages.${system}.cli
battery-up.packages.${system}.applet
battery-up.packages.${system}.full
```

Também é possível executar os apps expostos pela flake:

```sh
nix run github:lluz55/battery_up/v0.1.4#cli -- --once
nix run github:lluz55/battery_up/v0.1.4#applet
```

### Applet para COSMIC

O applet fica no pacote separado `.#applet`, registrado pelo arquivo desktop
`dev.lluz.BatteryUpApplet.desktop` com `X-CosmicApplet=true`. No COSMIC, depois
de instalar esse pacote no sistema, ele aparece como `Battery Up` na lista de
applets que podem ser adicionados ao painel/barra.

Para instalar CLI e applet juntos, use o pacote `.#full`.

O applet usa o arquivo de estado do daemon (`/var/lib/battery-up/state`) e mostra
o tempo acumulado com um ícone simbólico de bateria. Para usar outro arquivo de
estado durante testes:

```sh
BATTERY_UP_STATE_FILE=/tmp/battery-up-state cosmic-applet-battery-up
```

## Desenvolvimento

Entre no ambiente de desenvolvimento definido pelo `flake.nix`:

```sh
nix develop path:$PWD
```

Dentro do shell:

```sh
cargo test
cargo run -- --once
```

O workspace separa o core, o CLI e o applet:

```sh
cargo test -p battery-up-core -p battery-up
cargo build -p battery-up --profile release_cli
cargo build -p battery-up-cosmic-applet --profile release_applet
```

O perfil `release_cli` prioriza um binário pequeno para o CLI. O perfil
`release_applet` evita LTO e mantém mais paralelismo para reduzir o custo de
release da UI COSMIC.

## Regra de contagem

O tempo só é somado quando:

- existe uma bateria com `status = Discharging`
- nenhuma fonte `Mains` ou `USB` está com `online = 1`

Assim, carregador AC ou USB-C conectado interrompe a contagem.
