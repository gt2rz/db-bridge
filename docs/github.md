# Configuración de seguridad del repositorio en GitHub

Lo que vive en el repositorio (workflows con permisos mínimos y actions fijadas por SHA,
CODEOWNERS, Dependabot, `SECURITY.md`, plantillas, `deny.toml`) ya está en su sitio. Esta guía
cubre lo que **solo se puede activar desde la web**, en el orden recomendado.

Objetivo: nadie más que el propietario aprueba ni fusiona cambios en `main`; cualquiera puede
abrir issues y PRs; las releases solo salen de un commit aprobado y sus binarios son
verificables.

## 1. GitHub App para las releases (recomendado)

Sin ella, release-please abre su PR con el token del workflow: GitHub no ejecuta el CI sobre PRs
creadas así, y tendrías que fusionarla saltándote los checks obligatorios. Con la App, la PR la
crea la App, el CI se ejecuta y la apruebas tú como cualquier otra.

1. **Settings (de tu cuenta) → Developer settings → GitHub Apps → New GitHub App**
   - Nombre: `db-bridge-release` (debe ser único en GitHub). Homepage: la URL del repo.
   - Webhook: desmarca **Active**.
   - Repository permissions: **Contents: Read and write**, **Pull requests: Read and write**,
     **Issues: Read and write** (etiquetas de la PR). Nada más.
   - Where can this GitHub App be installed: **Only on this account**.
2. En la App: copia el **Client ID** y genera una **private key** (se descarga un `.pem`).
3. **Install App** → solo en `gt2rz/db-bridge`.
4. En el repo, **Settings → Secrets and variables → Actions**:
   - Variable `RELEASE_APP_CLIENT_ID` = el Client ID.
   - Secret `RELEASE_APP_PRIVATE_KEY` = el contenido completo del `.pem`. Después borra el
     archivo de tu disco.

## 2. Settings → General

- **Features**: desactiva *Wikis* y *Projects* si no los usas. *Issues* activado.
- **Pull Requests**:
  - Solo **Allow squash merging**, con *Default commit message* = **Pull request title**
    (el título, en Conventional Commits, es lo que lee release-please).
  - Desmarca *Allow merge commits* y *Allow rebase merging*.
  - Marca **Always suggest updating pull request branches** y
    **Automatically delete head branches**.

## 3. Settings → Actions → General

- **Actions permissions**: *Allow gt2rz, and select non-gt2rz, actions and reusable workflows*
  - Marca **Allow actions created by GitHub**.
  - En *Allow specified actions*:
    `Swatinem/rust-cache@*, googleapis/release-please-action@*, EmbarkStudios/cargo-deny-action@*`
  - Marca **Require actions to be pinned to a full-length commit SHA**.
- **Fork pull request workflows from outside collaborators**:
  **Require approval for all external contributors**. Así nadie ejecuta código en tus runners
  sin que tú lo apruebes.
- **Workflow permissions**: **Read repository contents and packages permissions** (cada job ya
  pide lo suyo de forma explícita).
- **Allow GitHub Actions to create and approve pull requests**:
  - Con la App del paso 1: **desmarcado**. Es lo más seguro: ningún workflow puede aprobar PRs.
  - Sin la App: marcado (release-please lo necesita para abrir su PR). La aprobación de un
    workflow no cuenta igualmente, porque el ruleset exige la revisión del code owner (tú).

## 4. Settings → Rules → Rulesets → New ruleset → Import a ruleset

Importa los dos archivos de `.github/rulesets/`:

- `main.json` — rama principal:
  - Nada de pushes directos, force push ni borrado; historial lineal; solo squash.
  - Toda PR necesita **tu aprobación como code owner**, con los hilos resueltos; un push nuevo
    invalida la aprobación anterior.
  - Check obligatorio: `CI`, con la rama al día. Es el job final de `ci.yml` y solo pasa si
    pasan (o se omiten) formato, `Comprobar (…)` en los tres sistemas y la auditoría; así, cambiar
    la matriz o saltarse la compilación en una PR de documentación no exige tocar el ruleset.
  - Bypass: rol **Admin**, solo a través de PR. Lo necesitas para fusionar tus propias PRs
    (GitHub no deja aprobar las propias) y, sin la App, la PR de release-please.
- `tags.json` — etiquetas `v*`: no se pueden mover ni borrar (las crea release-please; solo un
  admin puede saltárselo).

Comprueba después en **Insights → Rules insights** que se aplican.

## 5. Settings → Advanced Security (o *Code security*)

Activa:

- **Private vulnerability reporting** (lo usa `SECURITY.md`).
- **Dependency graph**, **Dependabot alerts**, **Dependabot security updates** y
  **Grouped security updates**.
- **Secret Protection** / **Secret scanning** y **Push protection**: bloquea pushes con tokens o
  claves.
- **Code scanning → CodeQL analysis → Default setup**: analiza los workflows (Actions) y el
  código Rust en cada PR y cada semana.

## 6. Settings → Moderation options (opcional)

- **Interaction limits**: si alguna vez hay spam, limita temporalmente quién comenta o abre
  issues.
- **Code review limits**: *Limit to users explicitly granted read or higher access* evita que
  aprobaciones de terceros aparezcan como revisiones.

## 7. Tu cuenta

- **Autenticación en dos pasos** activada (mejor con llave de seguridad o passkey).
- Opcional: **firma de commits** con tu clave SSH
  (`git config --global gpg.format ssh` + `user.signingkey`) y subirla en *SSH and GPG keys* como
  *Signing key*. Después puedes añadir la regla *Require signed commits* al ruleset de `main`.

## Qué NO activar

- **Immutable releases** (Settings → General → Releases): el workflow sube los binarios después
  de que release-please publique la Release, y con releases inmutables la subida fallaría.
  Si se quiere activar, hay que cambiar el flujo para que release-please cree la Release como
  borrador y se publique al terminar la subida.

## Flujo de trabajo resultante

1. Trabajas en una rama y abres PR con título convencional (`feat: …`).
2. El check `CI` pasa → la fusionas (squash) usando el bypass de admin.
3. release-please actualiza su PR de versión → la revisas, la apruebas y la fusionas.
4. Se crean la etiqueta y la Release, se compilan los binarios con su atestación y la landing
   se actualiza.

Dependabot y cualquier colaborador externo pasan por el mismo filtro: nada entra en `main` sin
tu aprobación.
