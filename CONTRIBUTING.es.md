# Contribuir a Omoikane

[English](CONTRIBUTING.md)

Gracias por querer echar una mano. Omoikane es una memoria a la que la gente confía lo que ha aprendido durante meses, en todos sus equipos y, en una empresa, compartida con todo un equipo. Eso marca todo lo que sigue: un cambio que pierde una memoria, la cuela de un cliente a otro o rompe la sincronización con una versión anterior es peor que una función que falta.

Quien participa sigue el [código de conducta](CODE_OF_CONDUCT.md) (en inglés).

## Antes de escribir código

- **Arreglos pequeños** (una errata, un mensaje más claro, un fallo de causa evidente): abre el pull request directamente.
- **Todo lo demás** (una función nueva, un comando o herramienta MCP nuevos, un cambio en la búsqueda o el recuerdo, cualquier cosa que toque la base de datos, la sincronización o el formato de exportación): abre antes una issue y cuenta qué problema quieres resolver. Así no trabajas en algo que choca con las reglas de abajo.
- **Problemas de seguridad**: no abras una issue pública. Mira [SECURITY.md](SECURITY.md).
- Las preguntas sobre cómo funciona algo también valen como issue; puede que ya las responda [docs/](docs/).

## Reglas que no se doblan

Salen de la promesa de la 1.0 («Estable desde la 1.0» en el README) y de lo que es el proyecto. Un pull request que rompa una no se fusiona, por bueno que sea en lo demás.

1. **La 1.x solo añade.** La base de datos (esquema 10), las líneas de sincronización y el formato de `omoi export` siguen siendo legibles para la 1.0. Un campo nuevo va en `extra` o con `#[serde(default)]`, y la 1.0 tiene que conservarlo y pasarlo. Lo que la 1.0 entendería mal sube `min_reader` / `lector_minimo` y necesita una issue antes.
2. **Nada cruza de un espacio a otro.** Una memoria, un veredicto, una lápida o una línea de proyecto de un cliente nunca llega al repositorio, la búsqueda o el recuerdo de otro. Los cambios aquí pasan una revisión extra que intenta romperlos.
3. **Ningún LLM dentro de Omoikane.** Funciona sin conexión, rápido e igual en todas las máquinas. El modelo de lenguaje lo pone el agente; Omoikane le da herramientas.
4. **Ocho herramientas MCP.** Cada herramienta de más le cuesta contexto a cada usuario en cada sesión. Lo nuevo va en parámetros de una herramienta que ya existe, o en la CLI.
5. **El recuerdo sigue siendo rápido.** El hook salta en cada mensaje. Solo por palabras, unos 30 ms (contando el arranque del proceso); con el modelo de significado, unos 32-54 ms con 3.000-10.000 memorias, y nunca más de 250 ms, pasados los cuales el recuerdo sigue sin él. Medido y no estimado: un cambio que frene cualquiera de los dos caminos dice cuánto.
6. **Local y privado.** Sin telemetría, sin cuentas, sin servidor nuestro. La red se usa solo para bajar el modelo, para actualizarse cuando el usuario ha dicho que sí y para sincronizar con el repositorio git del propio usuario.
7. **Windows, Linux y macOS por igual.** La CI pasa en los tres; las rutas, los finales de línea y el bloqueo de ficheros son las trampas de siempre.

## Compilar y probar

Hace falta Rust estable y git.

```bash
cargo build --release
cargo clippy --release --all-targets -- -D warnings
cargo test --release
```

- Los tests nunca tocan tu `~/.omoikane`: ponen `OMOIKANE_DB` y `OMOIKANE_HOME` en una carpeta temporal. Haz lo mismo cuando pruebes a mano, para que tu propia memoria quede fuera.
- `tests/equivalencia.rs` necesita el modelo int8 (unos 150 MB). Si `OMOI_MODELO_INT8` no apunta a él, se salta. La CI lo descarga.
- `tests/compat.rs` comprueba contra un `omoi` 1.0.0 de verdad. Apunta `OMOIKANE_COMPAT_BIN` a él (de la [Release v1.0.0](https://github.com/marcmayol/omoikane/releases/tag/v1.0.0)); sin eso, se salta. La CI lo pasa siempre, en los tres sistemas.
- Los secretos de mentira de los tests (tokens, claves) no pueden parecer reales enteros en el código, o la protección de GitHub bloquea el push. En `tests/secretos.rs` está cómo se parten.

## Cambios en la búsqueda, el recuerdo o el orden de resultados

Se juzgan con números, no con ejemplos que salen bien.

- Pasa el benchmark público ([omoikane-bench](https://github.com/marcmayol/omoikane-bench)) antes y después, y pon las dos tablas en el pull request.
- En el recall, pasa también el comparador de antes y después: `cargo build --release` y luego `cargo run --release --example eval -- tests/recall_casos.tsv --a-bin <omoi viejo> --b-bin <omoi nuevo>` (o un solo binario con `--a-env`/`--b-env`). Pasa los mismos mensajes por `omoi recall` con los dos, intercalados, y lista los casos que el cambio arregla y los que rompe. Puedes darle una copia de tu propia base con `--base` y tus propios casos; el formato del fichero está al principio de `examples/eval.rs`.
- Compara las dos versiones intercaladas (A, B, A, B…) en la misma máquina, nunca un bloque detrás de otro: las cachés y la temperatura hacen que el segundo bloque parezca mejor o peor de lo que es.
- La velocidad tiene una guardia en la CI (`perf.yml`, en cada push y antes de cada Release): baja el `omoi` de la última Release, lo comprueba como una actualización (la firma de `SHA256SUMS` y el SHA-256 del paquete) y mide `recall`, `search` desde un proyecto y `search --all` con los dos binarios en el mismo runner, intercalados, sobre la misma base sintética de 10.000 memorias. Falla si una mediana pasa a la vez un 25 % y 5 ms por encima de la de la Release, dos veces seguidas. Lo mismo en local: `cargo build --release` y luego `cargo run --release --example rendimiento` (`--a-bin <omoi>` para comparar con un binario que ya tengas, `--memorias`, `--rondas`, `--margen`; mira el principio de `examples/rendimiento.rs`). Si un cambio es más lento a propósito, di por qué en el pull request.
- Cuenta también lo que empeora. Un cambio que ayuda con las paráfrasis y mete ruido en las preguntas sin respuesta tiene que decirlo.

## Estilo del código

- El código está en español: nombres, comentarios y nombres de tests. Sigue el código que tienes alrededor. Si no escribes en español, pon los comentarios en inglés y lo resolvemos en la revisión; no renombres lo que ya existe.
- Comenta el porqué, no el qué, y solo cuando el motivo no es evidente.
- Ninguna dependencia nueva sin explicar por qué en el pull request. Las versiones de los crates de cifrado y red van fijadas (`=x.y.z`) a propósito.
- `cargo clippy -- -D warnings` tiene que pasar.

## Commits y pull requests

- Mensajes de commit en inglés, una línea que diga qué cambia para el usuario («Recall and search that scale: from 0.7 s to 70-110 ms with 100,000 memories, same results»). Los detalles, en el cuerpo.
- Un tema por pull request. Un arreglo, con un test que falla sin él.
- Apunta tu cambio en `CHANGELOG.md`, en **Unreleased**, en la sección que toca. Si toca la base de datos, la sincronización o la exportación, actualiza también **Compatibility**.
- Los textos que ve el usuario y la documentación, en los dos idiomas: si cambias `README.md` o un fichero de `docs/`, cambia también su `.es.md` (o di en el pull request que te hace falta ayuda con el español).
- Las versiones y las Releases las hace el mantenedor. Los arreglos suben el tercer número (1.0.1); el segundo es para funciones nuevas.

## Contribuciones hechas con IA

Bienvenidas: al fin y al cabo es una herramienta para agentes de programación. El autor sigues siendo tú: lee lo que envías, pasa los tests tú mismo y prepárate para explicar cualquier línea en la revisión. Un pull request que se nota que nadie ha leído se cierra.

## Licencia

Omoikane tiene [licencia MIT](LICENSE). Al enviar una contribución aceptas que se publique con la misma licencia.
