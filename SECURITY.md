# Política de seguridad

## Versiones con soporte

Solo la última versión publicada en [Releases](https://github.com/gt2rz/db-bridge/releases) recibe correcciones de seguridad.

## Cómo informar de una vulnerabilidad

**No abras un issue público.** Usa el formulario privado de GitHub: pestaña **Security → Report a vulnerability** de este repositorio. Solo el mantenedor puede verlo.

Incluye, si puedes, la versión, el sistema operativo, los pasos para reproducirlo y el impacto que ves. Recibirás respuesta en un plazo de 7 días; si se confirma, se publicará una corrección y un aviso de seguridad (GHSA) dando crédito a quien lo reportó, salvo que prefieras lo contrario.

## Verificar una descarga

Cada binario lleva una atestación de procedencia firmada por GitHub Actions y un `.sha256`:

```bash
gh attestation verify DB-Bridge-macOS-universal.dmg --repo gt2rz/db-bridge
shasum -a 256 -c DB-Bridge-macOS-universal.dmg.sha256
```

Si la atestación no verifica, no ejecutes el archivo y avisa por el formulario privado.
