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

Depois consulte o total com:

```sh
nix run . -- status
```

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

### Applet para COSMIC

O pacote também instala o applet `cosmic-applet-battery-up`, registrado pelo
arquivo desktop `dev.lluz.BatteryUpApplet.desktop` com `X-CosmicApplet=true`.
No COSMIC, depois de instalar o pacote no sistema, ele aparece como `Battery Up`
na lista de applets que podem ser adicionados ao painel/barra.

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

## Regra de contagem

O tempo só é somado quando:

- existe uma bateria com `status = Discharging`
- nenhuma fonte `Mains` ou `USB` está com `online = 1`

Assim, carregador AC ou USB-C conectado interrompe a contagem.
