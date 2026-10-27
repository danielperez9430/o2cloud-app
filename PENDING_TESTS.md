# Pruebas pendientes

Estado a 27 oct 2026. Entorno probado: macOS 27, Apple Silicon, una sola máquina.
La sincronización real entre dos Macs queda pendiente por falta de un segundo equipo;
el merge entre dispositivos se ha simulado con un snapshot y un `device_id` distintos.

## Ya validado (contexto)

- [x] Conflicto al subir a la raíz un archivo que ya estaba en otra carpeta.
- [x] Opción **Subir copias**: dos copias en O2 y entrada en la biblioteca.
- [x] Merge de un snapshot de un segundo dispositivo simulado (2 entradas / 2 dispositivos).
- [x] Reconciliación: poda de una copia fantasma y republicación del snapshot limpio.
- [x] Vista de Fotos: permiso, listado de 2.617 vídeos, badges y contador de ya subidos.
- [x] Creación y subida de snapshots a `/.o2cloud-gui/library/`.

## Pendiente

### 1. Subida real desde Fotos (UI)

- [ ] Abrir la app y conceder permiso de Fotos si macOS lo pide.
- [ ] Abrir «＋ Subir» → «Desde Fotos…».
- [ ] Elegir un vídeo pequeño de prueba y seleccionarlo.
- [ ] Pulsar «Subir seleccionados».
- [ ] Verificar:
  - [ ] aparece en Transferencias y termina en «Subido».
  - [ ] el archivo está en la carpeta destino de O2.
  - [ ] la biblioteca local registra una fuente `photos` con `captured_at`.
  - [ ] se sube un snapshot nuevo a `.o2cloud-gui/library`.
- [ ] Borrar el archivo de O2 y el snapshot de prueba; limpiar el estado local si se quiere dejar la biblioteca vacía.

Notas:

- Tras recompilar la app, macOS puede volver a pedir el permiso de Fotos. Es una
  limitación de la firma ad-hoc, no un fallo de la app.
- El diálogo de privacidad de macOS no acepta clics ni teclas sintéticas: hay que
  concederlo manualmente.
- No usar un vídeo personal sin querer subirlo de verdad.

### 2. Original solo en iCloud

- [ ] Elegir un vídeo que no esté descargado localmente (por ejemplo, con
      «Optimizar almacenamiento del Mac» activo).
- [ ] Subirlo desde Fotos.
- [ ] Verificar que la app espera la descarga de iCloud, muestra la fase
      «Descargando de Fotos…» y termina correctamente.
- [ ] Comprobar el tamaño/hash y que el original llega a O2.
- [ ] Comprobar el espacio libre: iCloud descarga el original completo antes de
      entregar datos, así que se necesita aproximadamente su tamaño libre.

### 3. Carpetas y archivos no multimedia

- [ ] Crear una carpeta local pequeña con un `.txt` y una foto o vídeo.
- [ ] Subirla con «＋ Subir» → «Carpeta…».
- [ ] Verificar la estructura remota.
- [ ] Confirmar que las carpetas siguen usando solo nombre + tamaño en la carpeta
      destino, sin preflight de hash.
- [ ] Subir un archivo no multimedia (`pdf`, `txt`…) individual y comprobar que su
      hash se registra después de subirlo, aunque no se precompruebe antes.

### 4. Sincronización real entre dos Macs

- [ ] En el Mac A, subir un archivo y sincronizar la biblioteca.
- [ ] En el Mac B, iniciar sesión y sincronizar.
- [ ] Comprobar que aparece el snapshot de A y que el `device_id` es distinto.
- [ ] Abrir Fotos en B y verificar que el archivo coincide por huella/hash.
- [ ] Intentar subir el mismo archivo en B y comprobar el diálogo de conflicto.
- [ ] Comprobar que los snapshots de ambos dispositivos se conservan.

### 5. Casos de biblioteca que faltan

- [ ] Probar «Saltar duplicados» con un caso real de Fotos (no solo archivo local).
- [ ] Borrar un archivo de O2 y ejecutar «Reconciliar con O2» desde la UI
      (el comando ya está probado).
- [ ] Borrar `.o2cloud-gui` desde la web de O2 y comprobar que la app la recrea;
      la biblioteca solo se reconstruye con lo nuevo.
- [ ] Snapshots incrementales/compactación (ahora se re-sube el snapshot completo).
- [ ] Olvidar/reconstruir la biblioteca.
- [ ] Elección por archivo en el conflicto (ahora se pregunta una vez por lote).
- [ ] Indexar hashes de manifiestos `.o2manifest.json` antiguos.
- [ ] Concurrencia: dos dispositivos subiendo a la vez.
- [ ] Relojes desajustados y periodo de gracia de la poda.
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

## Cómo limpiar después de una prueba

- Borrar el archivo o la carpeta de prueba de O2.
- Borrar los snapshots de prueba de `.o2cloud-gui/library`.
- Si se quiere dejar la biblioteca vacía, borrar:
  - `~/Library/Application Support/es.dabi.o2cloud/library.json`
  - `~/Library/Caches/es.dabi.o2cloud/library/`
- La papelera de O2 no se puede vaciar desde el CLI; hacerlo desde la web.
