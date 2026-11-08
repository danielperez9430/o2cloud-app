# Pruebas pendientes

Estado a 30 oct 2026. Entorno probado: macOS 27, Apple Silicon, una sola máquina.
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
- [x] Conflicto por archivo con dos vídeos de Fotos y con un PNG local.
- [x] Caché en disco del listado: raíz visible en 2 s con aviso de actualización.
- [x] Snapshots por delta y compactación: dos deltas y un snapshot completo final.
- [x] Olvidar biblioteca con snapshot real y reconstruirla con una subida nueva.
- [x] Indexar manifiestos antiguos: 3 vídeos grandes recuperados por hash.
- [x] Hash preflight opcional de archivos no multimedia y duplicados renombrados.
- [x] Filtros de Fotos por álbum, favoritos y rango de fechas.
- [x] Paginación de Fotos con páginas de 300 y «Mostrar más».
- [x] Miniaturas de Fotos bajo demanda en la lista.

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

### 3. Carpetas y archivos no multimedia — validada

- [x] Crear una carpeta local pequeña con un `.txt` y una foto o vídeo.
- [x] Subirla con «＋ Subir» → «Carpeta…».
- [x] Verificar la estructura remota.
- [x] Confirmar que las carpetas siguen usando solo nombre + tamaño en la carpeta
      destino, sin preflight de hash: una segunda subida de la misma carpeta se
      omitió por nombre+tamaño y el contenido remoto conservó la versión antigua.
- [x] Subir un archivo no multimedia individual y comprobar que su hash se registra
      después: `nuevo.txt` (41 B) quedó con `sha256: 2100e7e3…` y
      `source: "file"` en la biblioteca.
- [x] Hash preflight opcional de no multimedia: con «Hashear otros archivos
      antes de subir» activado, `original.txt` (1.800 B, `f31dcd34…`) subido a
      `/__HashTestA__` se detectó como `renamed.txt` al intentar subirlo a
      `/__HashTestB__`; el diálogo mostró la copia de A y «Saltar» no subió
      nada. Sin la opción se mantiene la comprobación por nombre+tamaño.

Resultado de la carpeta original: `/__Prueba3__/o2-prueba/` con `nota.txt`,
`icono.png` y `sub/otra.txt`; tamaños y hashes coinciden. La biblioteca registró
los tres como `source: "file"`. En la repetición,
`/__FolderTest__/o2-recreate/recrear.txt` se omitió por nombre+tamaño y
`nuevo.txt` individual registró su hash correctamente.

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
      antiguos también se limpian del mapa local. Probados dos deltas (746 y
      747 B) y la compactación a un snapshot de 1.370 B. Falta validarlo con
      dos equipos.
- [x] Olvidar/reconstruir la biblioteca: probado con un snapshot real. Olvidar
      dejó 0 entradas, `folder_id: null` y la carpeta remota vacía; una subida
      nueva recreó el snapshot y la entrada.
- [x] Elección por archivo en el conflicto: probado con dos vídeos de Fotos y
      con un PNG local; se subió solo el marcado.
- [x] Indexar hashes de manifiestos `.o2manifest.json` antiguos: encontrados
      `IMG_0112.MOV` (6,58 GB), `IMG_6114.MOV` (7,23 GB) y `IMG_4087.MOV`
      (4,40 GB) en `/Vídeos grandes`, con `source: "manifest"` y snapshot nuevo.
- [ ] Concurrencia: dos dispositivos subiendo a la vez (requiere dos equipos).
- [x] Periodo de gracia de la poda y reloj desajustado: probado con una copia
      reciente, otra con fecha futura y otra antigua; las dos primeras se
      conservan y la antigua se poda.
- [ ] Limpieza de snapshots antiguos en la papelera de O2: el CLI no puede
      vaciarla; queda como paso manual desde la web.

### 6. Fotos: pulido

- [x] Miniaturas: 64 px bajo demanda para las filas visibles, con el icono
      como respaldo si PhotoKit no tiene una miniatura local.
- [x] Cancelar una descarga de iCloud: botón «Cancelar» en la fila mientras
      PhotoKit exporta. No se pudo ver la fase con un original grande porque
      los probados ya estaban en caché; falta una prueba con descarga real.
- [x] Comprobar espacio libre antes de descargar un original: la app exige el
      tamaño del vídeo más 128 MB y falla la transferencia con un mensaje claro
      si no hay sitio. No se ha forzado un disco lleno en la prueba real.
- [x] Filtros por álbum, favoritos y rango de fechas: «Berlín» mostró 0 vídeos,
      «Solo favoritos» 9, y del 25/10/2026 al 25/10/2026 quedaron 3 vídeos
      (180 MB, 1 ya en O2). Al abrir Fotos, el rango se precarga del 29/09/2026
      al 29/10/2026; la casilla «Por fecha» filtra a 14 vídeos y «Limpiar»
      vuelve a los 300 de 2.619.
- [x] Paginación real: páginas de 300 desde PhotoKit; probado 300 → 600 con
      2.619 vídeos. Con «Fotos y vídeos» (52.017 elementos) se forzó una página
      de 10.100 y «Mostrar más» subió a 20.200 de 52.017, así que el offset
      >10.000 funciona. La carga inicial tarda ~1-2 min porque PhotoKit recorre
      toda la biblioteca para contar el total.
- [ ] Optimizar el conteo de Fotos: usar `PHFetchResult.count` y predicados en
      vez de recorrer los 52.017 elementos; hoy la primera página tarda ~1-2 min.

### 7. CLI, upstream y distribución

- [x] PR #8 revisada y mergeada, y release 0.8.0 con el fix de fechas negativas.
- [x] Sustituir el binario incluido por 0.8.0, borrar el parche y actualizar el README.
- [x] Issue #9 (salida JSON) implementada; la app usa `--json` en `status`, `upload`, `mkdir` y `quota`.
- [x] Probar con una cuenta real la migración a JSON: estado, cuota, crear carpeta,
      subir un archivo y publicar un snapshot de la biblioteca con CLI 0.8.0.
      Probado el 30 oct 2026: la app mostró 983 GB de 10,0 TB, creó una carpeta,
      subió un archivo (id 1553708755) y publicó un snapshot; los elementos de
      prueba se borraron después.
- [ ] Firma con Apple Developer ID y notarización.
- [ ] Builds y pruebas en Windows/Linux; CI.

### 8. Comprobaciones nuevas

- [ ] Subida de un vídeo de varios GB desde un disco externo lento: el hash
      previo se omite si no hay otro archivo del mismo tamaño, y la fila
      muestra el troceado con porcentaje en vez de quedarse parada.
- [ ] Archivo grande con otro del mismo tamaño en O2: sí se hashea, con
      progreso visible, para detectar el duplicado renombrado.
- [x] Caché en disco del listado: el listado guardado apareció en 2 s, mostró
      «Mostrando el último listado guardado. Actualizando…» y lanzó el `ls` en
      segundo plano.
- [x] Conflicto por archivo con archivos locales: `app-icon.png` detectado en
      `/__LocalConflict__` y copiado solo a `/__LocalConflict2__` al marcarlo.
- [x] «Subir todos» y «Saltar todos» probados en la UI: marcar y desmarcar
      cambia la casilla, y «Continuar» sin marcar no sube nada.
- [x] Olvidar biblioteca desde Ajustes con snapshot real y volver a sincronizar.
- [x] Sesión caducada: con un CLI simulado, crear carpeta devolvió
      `SESSION_EXPIRED` al instante, abrió el login y no ejecutó `mkdir`.
- [x] Ventana de login propia: se abre con el formulario de O2 y pulsar fuera
      (en la ventana principal) ya no la cierra; cerrarla muestra
      «Has cerrado la ventana de inicio de sesión.».
- [ ] Inicio de sesión completo con la ventana propia: SMS real y que el CLI
      valide las cookies y guarde la sesión.

## Cómo limpiar después de una prueba

- Borrar el archivo o la carpeta de prueba de O2.
- Borrar los snapshots de prueba de `.o2cloud-gui/library`.
- Si se quiere dejar la biblioteca vacía, borrar:
  - `~/Library/Application Support/es.dabi.o2cloud/library.json`
  - `~/Library/Caches/es.dabi.o2cloud/o2cloud/library/`
- La papelera de O2 no se puede vaciar desde el CLI; hacerlo desde la web.
