# PRD ; Longe

Nom : Longe (binaire `longe`, daemon `longed`, crate `longe`). Une longe laisse courir, impose le rayon et la durée.
Périmètre : produit complet, une seule version. Tout ce que décrit le transcript, rien de moins.
Source : YC Harness Club (Prime Agent, Continual Harness, Open Jarvis, QM).
Devise : le modèle est le moteur, le runtime est la voiture.

---

## 1. Objectif

Un binaire Rust unique qui donne à n'importe quel LLM (API ou local) un harnais auto-améliorant :

1. REPL persistant, un seul tool
2. Mémoire CRUD hors contexte, trois niveaux
3. Sub-agents persistants avec messagerie
4. Budgets imposés (grind)
5. Vérification externe obligatoire
6. Sandbox par processus
7. Réflexion post-run : le runtime réécrit ses propres skills, prompt et config
8. Daemon : sessions survivent à la fermeture du terminal, crons
9. Cockpit : observer, intervenir, depuis n'importe où

## 2. Hors périmètre

- Fine-tuning, LoRA, test-time training
- UI web riche, Slack (le cockpit est une TUI + un endpoint HTTP JSON)
- Multi-machine (un seul hôte ; NATS non requis)

## 3. Architecture

```
┌─ daemon ────────────────────────────────────────────────────┐
│  scheduler ── cron ── budget guard ── reflect job            │
│      │                                                       │
│      ▼                                                       │
│  session tree                                                │
│   root ──┬── child A (idle, état en RAM)                     │
│          ├── child B (running)                               │
│          └── child C (offloaded, sérialisé sur disque)       │
│      │         ▲                                             │
│      │         └── bus messages (parent/enfant/fratrie)      │
│      ▼                                                       │
│  session = { history, lua state, sandbox, budget }           │
│      │                                                       │
│      ▼                                                       │
│  context compiler → llm client → parser → lua repl           │
│                                              │               │
│                        verifier ◄────────────┘               │
│                                                              │
│  store (fs) : prompt.md skills/ memory/ subagents/ sessions/ │
│               trajectories/ harness.toml                     │
└──────────────────────────────────────────────────────────────┘
       ▲
       │ unix socket + HTTP JSON
  cockpit TUI / curl / tailscale
```

## 4. Composants

### 4.1 REPL : un seul tool `exec(code)`

Lua 5.4 embarqué (mlua). L'état survit entre les appels et entre les redémarrages (sérialisation à chaque tour).

Bindings natifs exposés dans Lua :

| Namespace | Fonctions |
|---|---|
| `fs` | `read(p)` `write(p,s)` `list(d)` `rm(p)` ; borné au workspace de la session |
| `sh` | `sh(cmd, {timeout})` → `{stdout, stderr, code}` ; exécuté dans la sandbox |
| `mem` | `get(k)` `set(k,s)` `del(k)` `list()` `search(q)` |
| `skill` | `get(n)` `set(n,s)` `del(n)` `list()` |
| `prompt` | `get()` `set(s)` |
| `agent` | `spawn(name, task, {model, budget})` → id ; `send(id, msg)` ; `recv()` → messages ; `list()` ; `pause(id)` ; `resume(id)` ; `kill(id)` |
| `llm` | `query(prompt, {model})` ; appel LLM sans état, feuille du RLM |
| `model` | `switch(provider, name)` ; changement de runtime en cours de session |
| `compact` | `compact(hint)` ; compaction déclenchée par le modèle |
| `verify` | `verify()` → `{ok, report}` |
| `note` | `note(s)` ; trace humaine hors contexte |
| `done` | `done(summary)` ; refusé si budget min ou verify KO |

Retour de `exec` : stdout + valeur, tronqué à 8 ko ; le reste reste dans `_last`.

### 4.2 Mémoire trois niveaux

- **L1 contexte** : history de la session. Compaction auto à 70 % de la fenêtre ; le modèle peut aussi l'appeler. Résumé structuré remplace l'historique, on garde system + résumé + 5 derniers tours.
- **L2 état Lua** : variables, fonctions, données. Sérialisé chaque tour dans `sessions/<id>/state.lua`. Garbage collection agentique : le modèle voit la taille de son état et peut nettoyer.
- **L3 store** : `memory/` `skills/` `prompt.md` `subagents/` `trajectories/`. CRUD complet. Index injecté dans le system prompt (noms + première ligne).

### 4.3 Sub-agents

- `agent.spawn` crée une session enfant avec son propre workspace, état Lua, budget, modèle.
- Trois états : `running`, `idle` (état en RAM, réveillable par message), `offloaded` (sérialisé sur disque, rechargé à la réception d'un message).
- Bus : parent ↔ enfant, et fratrie. Messages en file, lus via `agent.recv()` ou injectés dans le contexte au tour suivant.
- Fin de tâche : l'enfant envoie un rapport au parent, passe `idle`.
- Le modèle décide seul de spawner ; le system prompt l'y encourage sur les tâches parallélisables.

### 4.4 Budget guard (grind)

```toml
[budget]
min_seconds = 1800
min_turns   = 20
max_turns   = 400
max_tokens  = 4_000_000
```

`done()` avant le minimum est refusé avec compteur. Budget épuisé : sauvegarde forcée et sortie propre.

### 4.5 Verifier

Commande configurable par projet (`cargo test`, `cargo clippy -- -D warnings`, `cargo mutants`, script IRONLOOP). Rapport renvoyé au modèle. `done()` refusé si dernier `verify()` KO.

### 4.6 Sandbox (modèle Codex)

Trois modes, une politique commune, un backend natif par OS derrière un trait. Rien n'est réimplémenté : ce sont les primitives du noyau.

```rust
enum Mode { ReadOnly, WorkspaceWrite, FullAccess }

struct Policy {
    mode: Mode,
    workspace: PathBuf,
    read_extra: Vec<PathBuf>,   // ~/.cargo, toolchains
    network: bool,              // false par défaut
    timeout: Duration,
}

trait Sandbox { fn run(&self, p: &Policy, cmd: &str) -> Result<Output>; }

#[cfg(target_os = "linux")]   type Native = LandlockSeccomp;   // crates landlock + extrasafe, dans pre_exec
#[cfg(target_os = "macos")]   type Native = Seatbelt;          // profil généré, sandbox-exec -p
#[cfg(target_os = "windows")] type Native = RestrictedToken;   // CreateRestrictedToken + Job Object
```

| Mode | Écriture | Lecture | Réseau | Usage |
|---|---|---|---|---|
| `ReadOnly` | aucune | workspace + toolchain | non | verifier, runs de fitness |
| `WorkspaceWrite` | workspace, /tmp | workspace + toolchain | config | sessions (défaut) |
| `FullAccess` | tout | tout | oui | `--unsafe-full-access` explicite, container dédié uniquement |

Règles fixes quel que soit le mode : `~/.ssh`, `.env`, le store du runtime (`prompt.md`, `skills/`, `memory/`, `evals/`) et le binaire lui-même sont invisibles ou en lecture seule pour `sh()`. Le modèle modifie le store uniquement via les bindings Lua, donc via du Rust.

Séquence : ce soir `FullAccess` dans un container dédié avec séparation d'utilisateur Unix (runtime en root, `sh()` en user `agent`, `evals/` et `store/` en root lecture seule). Backends natifs livrés ensuite dans l'ordre Linux, macOS, Windows.

### 4.7 Réflexion post-run (continual harness)

À chaque `done()` réussi et sur cron :

1. Le runtime charge la trajectoire complète (`trajectories/<id>.jsonl`).
2. Session de réflexion, prompt dédié : "qu'est-ce qui a marché, échoué, coûté cher ; propose des diffs sur skills/, prompt.md, subagents/, harness.toml".
3. Les diffs sont appliqués dans une branche git du store.
4. Fitness : rejoue un jeu de tâches de référence (`evals/`) avec le nouveau store ; on garde si score ≥ précédent.
5. Sinon rollback. Tout est loggé.

Le humain peut forcer accept/reject depuis le cockpit.

### 4.8 Daemon et crons

- Le binaire se lance en daemon (`longed`) ; la TUI et la CLI s'y connectent par socket unix.
- `Ctrl-C` ferme le client, pas la session.
- Crons : `harness.toml` déclare des tâches récurrentes (réflexion, veille, nettoyage).

### 4.9 Cockpit

- TUI (ratatui) : arbre des sessions, état, budget consommé, dernier `note`, tail de la trajectoire, envoi de message à n'importe quelle session.
- HTTP JSON sur `127.0.0.1:port` : `/sessions`, `/sessions/:id`, `/sessions/:id/message`, `/store/diffs`. Exposable par tailscale.

### 4.10 Client LLM

Providers : Anthropic Messages, OpenAI-compatible (Ollama, DeepSeek, OpenRouter). Trait commun, streaming, retry, comptage tokens. Switch à chaud via `model.switch`.

## 5. Boucle d'une session

```
load store + state.lua
system = prompt.md + index(skills) + index(memory) + index(subagents) + budget status
loop:
  drain bus → push messages reçus
  ctx = compile(history)
  resp = llm(ctx)
  match parse(resp):
    Exec(code) → out = lua.exec(code); push(out); persist state
    Done(s)    → budget.ok() && verify.ok() ? finish : push(refus)
    Text       → push("Réponds via exec ou done.")
  ctx.tokens > 0.7 * window → compact()
  budget.exhausted() → finish(forced)
finish: rapport au parent, trajectoire écrite, état → idle, reflect job planifié
```

## 6. Stack

| Besoin | Crate |
|---|---|
| async | tokio |
| HTTP client | reqwest (rustls) |
| SSE | eventsource-stream |
| Lua | mlua (`lua54`, `vendored`, `send`, `serialize`) |
| sandbox | landlock, extrasafe (Linux) ; windows (Windows) ; sandbox-exec via Command (macOS) |
| TUI | ratatui + crossterm |
| HTTP server | axum |
| config | serde, toml |
| CLI | clap |
| git store | gix |
| erreurs | anyhow, thiserror |
| logs | tracing |

Un seul binaire. Pas de framework d'agents. Store = fichiers plats versionnés par git.

## 7. Arborescence

```
src/
  main.rs
  daemon.rs        # socket, scheduler, cron
  session/         # mod.rs tree.rs state.rs bus.rs
  loop.rs
  llm/             # provider.rs anthropic.rs openai.rs
  repl/            # mod.rs bindings/*.rs
  store.rs
  compact.rs
  budget.rs
  verify.rs
  sandbox/         # mod.rs policy.rs linux.rs macos.rs windows.rs
  reflect.rs
  cockpit/         # tui.rs http.rs
  parse.rs
store/
  prompt.md skills/ memory/ subagents/ sessions/ trajectories/ evals/ harness.toml
```

## 8. Test de référence : jq en Rust (program bench réduit)

Tâche : "Implémente `rjq`, un clone de `jq` en Rust, stdlib only. Binaire `rjq <filtre> < entrée.json`, sortie JSON compacte sur stdout, code de retour 0/1/5 comme jq. Découpe en sous-tâches si utile."

### Jeu d'évaluation

`evals/` (root, lecture seule pour `sh()`), ~700 cas générés une fois avec le vrai `jq` par `gen-evals.sh` (self-check 693/693 avec `jq -c`) :

```
evals/cases/<n>/filter        # ex : .[] | select(.age > 30) | .name
evals/cases/<n>/input.json
evals/cases/<n>/expected      # sortie de jq -c
evals/cases/<n>/exit          # code de retour attendu
evals/cases/<n>/tier          # palier 1 à 6
evals/run.sh <bin> [tier]     # imprime "N/693 OK" + une ligne par échec (filtre, attendu, obtenu, exit)
```

Couverture par paliers, pour mesurer la progression :
1. accès : `.`, `.a`, `.a.b`, `.[0]`, `.[]`, `.[2:5]`, `.a?`
2. construction : `{a, b: .x}`, `[.[] | .y]`, `..`
3. pipes et filtres : `|`, `,`, `select`, `map`, `keys`, `length`, `has`, `type`
4. arithmétique et comparaison : `+ - * /`, `== != < >`, `and or not`, `//`
5. fonctions : `sort_by`, `group_by`, `unique`, `to_entries`, `from_entries`, `add`, `range`, `tostring`, `tonumber`, `split`, `join`, `test`
6. erreurs : filtre invalide → exit 5 ; JSON invalide → exit 2 ; `error("x")` → exit 5

### verify()

```
cargo test && cargo clippy -- -D warnings && evals/run.sh
```
`done()` accepté uniquement à 693/693. Le rapport renvoyé au modèle liste les cas en échec avec filtre, attendu, obtenu.

### Protocole

- **A (baseline)** : boucle + tools JSON (read, write, sh). Pas de budget, pas de mémoire, pas de sub-agents.
- **B (Longe)** : tout activé. Budget min 60 min, max 400 tours, max 6M tokens.
- Même modèle (DeepSeek V4 Pro), même température, même prompt de tâche.
- Second run de B : "Ajoute les options CLI `-r`, `-s`, `-n`, `-e`, `--arg`, `--argjson`, `--tab`, et `input`/`inputs` sur plusieurs documents" avec un second jeu généré de la même façon ; on regarde si `skills/` et `memory/` ont servi (moins de tours sur le parseur, réutilisation de l'AST).

### Métriques

| Métrique | A | B | B run 2 |
|---|---|---|---|
| cas verts / total | | | |
| tokens | | | |
| tours | | | |
| temps mural | | | |
| `done` refusés | | | |
| sub-agents spawnés | | | |
| diffs reflect proposés / acceptés | | | |

Hypothèse validée si B ≥ A en cas verts à tokens égaux ou inférieurs, et si B run 2 consomme moins de tours que B run 1 sur les paliers déjà couverts.

## 9. Critères d'acceptation

- [ ] un seul binaire `longe`, `cargo build --release` ; `longe daemon` lance `longed`
- [ ] état Lua persistant entre tours et entre redémarrages du daemon
- [ ] `mem`, `skill`, `prompt` : CRUD visible après redémarrage
- [ ] `agent.spawn` + `send` + `recv` fonctionnels ; enfant `offloaded` réveillé par message
- [ ] `done()` refusé avant budget min et si verify KO
- [ ] compaction sans perte de la consigne initiale
- [ ] sandbox ce soir : `sh("cat /opt/agent/evals/*")` et `sh("echo x > /opt/agent/store/prompt.md")` échouent (permissions Unix)
- [ ] sandbox natif (livraison suivante) : trait `Sandbox` en place, `WorkspaceWrite` refuse `sh("cat ~/.ssh/id_rsa")` et `sh("curl example.com")` sur Linux
- [ ] `model.switch` en cours de session sans perte d'état
- [ ] `Ctrl-C` client ; session toujours vivante dans le daemon
- [ ] réflexion post-run produit au moins un diff, fitness le tranche, git log le montre
- [ ] cockpit TUI affiche l'arbre et permet d'envoyer un message à une session enfant
- [ ] deux providers passent le test section 8 (DeepSeek V4 Pro via API, un modèle local via Ollama)
