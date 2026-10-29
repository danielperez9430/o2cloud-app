# Pruebas pendientes

Estado a 29 oct 2026. Entorno probado: macOS 27, Apple Silicon, una sola máquina.
La sincronización real entre dos Macs queda pendiente por falta de un segundo equipo;
el merge entre dispositivos se ha simulado con un snapshot y un `device_id` distintos.

## Ya validado (contexto)

- [x] Conflicto al subir a la raíz un archivo que ya estaba en otra carpeta.
- [x] Opción **Subir copias**: dos copias en O2 y entrada en la biblioteca.
- [x] Merge de un snapshot de un segundo dispositivo simulado (2 entradas / 2 dispositivos).
- [x] Reconciliación: poda de una copia fantasma y republicación del snapshot limpio.
- [x] Vista de Fotos: permiso, listado de 2.617 vídeos, badges y contador de ya subidos.
- [x] Creación y subida de snapshots a `/.o2cloud-gui/library/`.
- [x] Subida real desde Fotos, incluyendo un original que solo estaba en iCloud.
- [x] Subida de una carpeta con `.txt`, imagen y subcarpeta; estructura, tamaños y hashes.
- [x] Conflicto por archivo: dos duplicados, se sube solo el marcado.
- [x] Borrar `.o2cloud-gui` y comprobar que la app la recrea con una subida nueva.
- [x] Reconciliar desde la UI tras borrar copias en O2.
- [x] Periodo de gracia de la poda: las entradas recientes se conservan y las antiguas se podan.

## Pendiente

### 1. Subida real desde Fotos (UI) — validada 28 oct 2026

- [x] Abrir la app y conceder permiso de Fotos si macOS lo pide.
- [x] Abrir «＋ Subir» → «Desde Fotos…».
- [x] Elegir un vídeo pequeño de prueba y seleccionarlo.
- [x] Pulsar «Subir seleccionados».
- [x] Verificar:
  - [x] aparece en Transferencias y termina en «Subido».
  - [x] el archivo está en la carpeta destino de O2.
  - [x] la biblioteca local registra una fuente `photos` con `captured_at`.
  - [x] se sube un snapshot nuevo a `.o2cloud-gui/library`.

Resultado: `ScreenRecording_10-25-2026 22-29-20_1.mp4`, 33.850.613 bytes,
subido a `/__Test3__` y con el mismo tamaño en O2 (id 1551013127).

Notas:

- Tras recompilar la app, macOS puede volver a pedir el permiso de Fotos. Es una
  limitación de la firma ad-hoc, no un fallo de la app. `app.sh` firma el bundle
  completo para que TCC pueda atribuir el permiso.
- El diálogo de privacidad de macOS no acepta clics ni teclas sintéticas: hay que
  concederlo manualmente.
- No usar un vídeo personal sin querer subirlo de verdad.

### 2. Original solo en iCloud — validada 28 oct 2026

- [x] Elegir un vídeo que no esté descargado localmente.
- [x] Subirlo desde Fotos.
- [x] Verificar que la app espera la descarga de iCloud y termina correctamente.
- [x] Comprobar el tamaño y que el original llega a O2.
- [x] Comprobar el espacio libre: se necesitaba aproximadamente su tamaño libre.

Resultado: `99BC9A63-3F56-4A44-88C7-196EE3F16196.mp4`, 21.823.721 bytes.
No estaba entre los originales locales de la fototeca. La descarga fue rápida y
no dio tiempo a capturar la fase «Descargando de Fotos…», pero el archivo llegó
completo y con el tamaño correcto.

### 3. Carpetas y archivos no multimedia — parcial

- [x] Crear una carpeta local pequeña con un `.txt` y una foto o vídeo.
- [x] Subirla con «＋ Subir» → «Carpeta…».
- [x] Verificar la estructura remota.
- [ ] Confirmar que las carpetas siguen usando solo nombre + tamaño en la carpeta
      destino, sin preflight de hash. No se repitió la subida para comprobar el salto.
- [ ] Subir un archivo no multimedia individual y comprobar que su hash se registra
      después. Los `.txt` de la carpeta sí quedaron registrados con `source: "file"`,
      pero no se probó un archivo individual.

Resultado de la carpeta: `/__Prueba3__/o2-prueba/` con `nota.txt`, `icono.png` y
`sub/otra.txt`; tamaños y hashes coinciden. La biblioteca registró los tres como
`source: "file"`.

### 4. Sincronización real entre dos Macs

- [ ] En el Mac A, subir un archivo y sincronizar la biblioteca.
- [ ] En el Mac B, iniciar sesión y sincronizar.
- [ ] Comprobar que aparece el snapshot de A y que el `device_id` es distinto.
- [ ] Abrir Fotos en B y verificar que el archivo coincide por huella/hash.
- [ ] Intentar subir el mismo archivo en B y comprobar el diálogo de conflicto.
- [ ] Comprobar que los snapshots de ambos dispositivos se conservan.

### 5. Casos de biblioteca

- [x] Probar «Saltar duplicados» con un caso real de Fotos.
- [x] Borrar copias en O2 y ejecutar «Reconciliar con O2» desde la UI.
- [x] Borrar `.o2cloud-gui` y comprobar que la app la recrea con una subida nueva.
- [x] Snapshots incrementales/compactación: cada lote sube un delta; tras 8
      snapshots propios se sube uno completo y se borran los antiguos. Los IDs
      antiguos también se limpian del mapa local. Falta validarlo con dos equipos.
- [x] Olvidar/reconstruir la biblioteca: implementado el 29 oct; probado con
      biblioteca vacía. Falta probar con snapshots en O2 y resincronizar.
- [x] Elección por archivo en el conflicto: probado con dos vídeos de Fotos; se
      subió solo el marcado. Falta probar la ruta de archivos locales.
- [ ] Indexar hashes de manifiestos `.o2manifest.json` antiguos.
- [ ] Concurrencia: dos dispositivos subiendo a la vez.
- [x] Periodo de gracia de la poda. No se ha simulado un reloj desajustado.
- [ ] Limpieza de snapshots antiguos en la papelera de O2.

### 6. Fotos: pulido

- [ ] Miniaturas.
- [ ] Cancelar una descarga de iCloud.
- [ ] Comprobar espacio libre antes de descargar un original.
- [ ] Filtros por álbum, favoritos o rango de fechas.
- [ ] Más de 10.000 elementos / paginación real.

### 7. CLI, upstream y distribución

- [ ] PR #8 revisada y mergeada, y release con el fix de fechas negativas.
- [ ] Sustituir el binario incluido, borrar el parche y actualizar el README.
- [ ] Issue #9 (salida JSON) implementada; dejar de parsear la salida humana.
- [ ] Firma con Apple Developer ID y notarización.
- [ ] Builds y pruebas en Windows/Linux; CI.

### 8. Comprobaciones nuevas

- [ ] Caché en disco del listado: arrancar con el último listado guardado,
      comprobar el aviso «Actualizando…» y que se sustituye al terminar el `ls`.
- [ ] Conflicto por archivo con archivos locales, no solo Fotos.
- [ ] Botones «Saltar todos» / «Subir todos» del diálogo de conflicto.
- [ ] Olvidar biblioteca desde Ajustes con snapshots en O2 y volver a sincronizar.
- [ ] Sesión caducada: una operación que muta (por ejemplo, crear carpeta) puede
      esperar a la ventana de login del CLI. Revisar timeout/mensaje.

## Cómo limpiar después de una prueba

- Borrar el archivo o la carpeta de prueba de O2.
- Borrar los snapshots de prueba de `.o2cloud-gui/library`.
- Si se quiere dejar la biblioteca vacía, borrar:
  - `~/Library/Application Support/es.dabi.o2cloud/library.json`
  - `~/Library/Caches/es.dabi.o2cloud/o2cloud/library/`
- La papelera de O2 no se puede vaciar desde el CLI; hacerlo desde la web.
