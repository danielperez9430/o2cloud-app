// PhotoKit bridge for the "Subir desde Fotos" view.
//
// The app is the process macOS asks for Photos access, so everything runs in
// this process instead of in a helper binary. Rust calls the C functions
// below; strings returned here are malloc'd and must be released with
// o2_photos_free.

#import <Foundation/Foundation.h>
#import <Photos/Photos.h>

#include <stdlib.h>
#include <string.h>

typedef void (*O2PhotosProgressCallback)(
    void *context,
    unsigned long long bytes_written,
    unsigned long long bytes_total
);

enum {
    O2PhotosStatusNotDetermined = 0,
    O2PhotosStatusRestricted = 1,
    O2PhotosStatusDenied = 2,
    O2PhotosStatusAuthorized = 3,
    O2PhotosStatusLimited = 4,
};

static int O2PhotosMapStatus(PHAuthorizationStatus status) {
    switch (status) {
        case PHAuthorizationStatusNotDetermined:
            return O2PhotosStatusNotDetermined;
        case PHAuthorizationStatusRestricted:
            return O2PhotosStatusRestricted;
        case PHAuthorizationStatusDenied:
            return O2PhotosStatusDenied;
        case PHAuthorizationStatusAuthorized:
            return O2PhotosStatusAuthorized;
        case PHAuthorizationStatusLimited:
            return O2PhotosStatusLimited;
        default:
            return -1;
    }
}

static PHAuthorizationStatus O2PhotosCurrentStatus(void) {
    if (@available(macOS 11.0, *)) {
        return [PHPhotoLibrary authorizationStatusForAccessLevel:PHAccessLevelReadWrite];
    }
    return [PHPhotoLibrary authorizationStatus];
}

static BOOL O2PhotosHasAccess(void) {
    PHAuthorizationStatus status = O2PhotosCurrentStatus();
    return status == PHAuthorizationStatusAuthorized || status == PHAuthorizationStatusLimited;
}

static char *O2PhotosCopyString(NSString *value) {
    if (value == nil) {
        return NULL;
    }
    const char *utf8 = value.UTF8String;
    if (utf8 == NULL) {
        return NULL;
    }
    return strdup(utf8);
}

static NSString *O2PhotosJSONString(id object) {
    NSError *error = nil;
    NSData *data = [NSJSONSerialization dataWithJSONObject:object options:0 error:&error];
    if (data == nil) {
        return nil;
    }
    return [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
}

static char *O2PhotosJSONError(NSString *message) {
    NSString *text = message ?: @"Error desconocido de Fotos";
    NSString *json = O2PhotosJSONString(@{@"error": text});
    return O2PhotosCopyString(json ?: text);
}

/// 4 = the original full-size video, 3 = the current video, and so on.
/// The name and the size of a PHAsset come from its resource, not from the
/// asset itself: PHAssetResource exposes the original filename, and macOS
/// also exposes the original byte size through its `fileSize` key.
static NSInteger O2PhotosResourceRank(PHAssetResourceType type) {
    switch (type) {
        case PHAssetResourceTypeFullSizeVideo:
            return 4;
        case PHAssetResourceTypeVideo:
            return 3;
        case PHAssetResourceTypeFullSizePhoto:
            return 2;
        case PHAssetResourceTypePhoto:
            return 1;
        default:
            return 0;
    }
}

static unsigned long long O2PhotosResourceSize(PHAssetResource *resource) {
    if (resource == nil) {
        return 0;
    }
    @try {
        id value = [resource valueForKey:@"fileSize"];
        if ([value isKindOfClass:[NSNumber class]]) {
            long long size = [value longLongValue];
            if (size > 0) {
                return (unsigned long long)size;
            }
        }
    } @catch (NSException *exception) {
        // The key is undocumented; treat it as unknown if macOS stops
        // exposing it.
    }
    return 0;
}

static PHAssetResource *O2PhotosBestResource(PHAsset *asset) {
    NSArray<PHAssetResource *> *resources = [PHAssetResource assetResourcesForAsset:asset];
    PHAssetResource *best = nil;
    NSInteger bestRank = -1;
    unsigned long long bestSize = 0;

    for (PHAssetResource *resource in resources) {
        NSInteger rank = O2PhotosResourceRank(resource.type);
        if (rank == 0) {
            continue;
        }
        unsigned long long size = O2PhotosResourceSize(resource);
        if (rank > bestRank || (rank == bestRank && size > bestSize)) {
            best = resource;
            bestRank = rank;
            bestSize = size;
        }
    }

    // Unusual resources (audio, adjustments, paired Live Photo videos) still
    // have an original file. Fall back to the largest one.
    if (best == nil) {
        for (PHAssetResource *resource in resources) {
            unsigned long long size = O2PhotosResourceSize(resource);
            if (size > bestSize) {
                best = resource;
                bestSize = size;
            }
        }
    }
    return best;
}

static NSDictionary *O2PhotosAssetDictionary(PHAsset *asset) {
    PHAssetResource *resource = O2PhotosBestResource(asset);
    NSString *name = resource.originalFilename;
    if (name.length == 0) {
        name = asset.localIdentifier.lastPathComponent;
    }
    if (name.length == 0) {
        name = @"archivo";
    }

    long long date = 0;
    if (asset.creationDate != nil) {
        date = (long long)llround(asset.creationDate.timeIntervalSince1970 * 1000.0);
    }

    NSString *media_type = @"other";
    if (asset.mediaType == PHAssetMediaTypeVideo) {
        media_type = @"video";
    } else if (asset.mediaType == PHAssetMediaTypeImage) {
        media_type = @"photo";
    }

    return @{
        @"id": asset.localIdentifier ?: @"",
        @"name": name,
        @"size": @(O2PhotosResourceSize(resource)),
        @"date": date > 0 ? @(date) : [NSNull null],
        @"duration": @(asset.mediaType == PHAssetMediaTypeVideo ? asset.duration : 0.0),
        @"media_type": media_type,
        @"width": @(asset.pixelWidth),
        @"height": @(asset.pixelHeight),
    };
}

int o2_photos_authorization_status(void) {
    return O2PhotosMapStatus(O2PhotosCurrentStatus());
}

int o2_photos_request_access(void) {
    dispatch_semaphore_t semaphore = dispatch_semaphore_create(0);
    __block int result = -1;

    // The permission alert is UI: PhotoKit expects this call on the main
    // thread. The Rust command runs it on a worker, so hop to the main queue
    // and block that worker until the user answers.
    dispatch_async(dispatch_get_main_queue(), ^{
        if (@available(macOS 11.0, *)) {
            [PHPhotoLibrary requestAuthorizationForAccessLevel:PHAccessLevelReadWrite
                                                      handler:^(PHAuthorizationStatus status) {
                result = O2PhotosMapStatus(status);
                dispatch_semaphore_signal(semaphore);
            }];
        } else {
            [PHPhotoLibrary requestAuthorization:^(PHAuthorizationStatus status) {
                result = O2PhotosMapStatus(status);
                dispatch_semaphore_signal(semaphore);
            }];
        }
    });

    // Do not leave the button on "Comprobando…" forever if macOS never
    // presents the alert.
    if (dispatch_semaphore_wait(
            semaphore,
            dispatch_time(DISPATCH_TIME_NOW, 120 * NSEC_PER_SEC)
        ) != 0) {
        return -1;
    }
    return result;
}

char *o2_photos_albums_json(void) {
    @autoreleasepool {
        if (!O2PhotosHasAccess()) {
            return O2PhotosJSONError(@"O2 Cloud no tiene permiso para leer Fotos.");
        }
        PHFetchOptions *options = [[PHFetchOptions alloc] init];
        options.sortDescriptors = @[
            [NSSortDescriptor sortDescriptorWithKey:@"localizedTitle" ascending:YES],
        ];
        PHFetchResult<PHAssetCollection *> *collections =
            [PHAssetCollection fetchAssetCollectionsWithType:PHAssetCollectionTypeAlbum
                                                      subtype:PHAssetCollectionSubtypeAny
                                                      options:options];
        NSMutableArray<NSDictionary *> *albums = [NSMutableArray array];
        for (PHAssetCollection *album in collections) {
            NSString *title = album.localizedTitle;
            if (title.length == 0) {
                continue;
            }
            [albums addObject:@{
                @"id": album.localIdentifier ?: @"",
                @"title": title,
            }];
        }
        return O2PhotosCopyString(O2PhotosJSONString(@{@"albums": albums}));
    }
}

char *o2_photos_list_json(
    int media_type,
    unsigned long long min_size,
    unsigned int limit,
    const char *album_id,
    int favorites_only,
    long long start_ms,
    long long end_ms
) {
    @autoreleasepool {
        if (!O2PhotosHasAccess()) {
            return O2PhotosJSONError(@"O2 Cloud no tiene permiso para leer Fotos.");
        }
        if (limit == 0) {
            limit = 10000;
        }

        PHFetchOptions *options = [[PHFetchOptions alloc] init];
        options.sortDescriptors = @[
            [NSSortDescriptor sortDescriptorWithKey:@"creationDate" ascending:NO],
        ];
        options.includeHiddenAssets = NO;
        options.includeAllBurstAssets = NO;

        PHFetchResult<PHAsset *> *fetch = nil;
        NSString *album_identifier = nil;
        if (album_id != NULL) {
            album_identifier = [NSString stringWithUTF8String:album_id];
        }
        if (album_identifier.length > 0) {
            PHFetchResult<PHAssetCollection *> *collections =
                [PHAssetCollection fetchAssetCollectionsWithLocalIdentifiers:@[album_identifier]
                                                                      options:nil];
            PHAssetCollection *album = collections.firstObject;
            if (album != nil) {
                fetch = [PHAsset fetchAssetsInAssetCollection:album options:options];
            }
        }
        if (fetch == nil) {
            if (media_type == PHAssetMediaTypeImage || media_type == PHAssetMediaTypeVideo) {
                fetch = [PHAsset fetchAssetsWithMediaType:(PHAssetMediaType)media_type
                                                  options:options];
            } else {
                fetch = [PHAsset fetchAssetsWithOptions:options];
            }
        }

        NSMutableArray<NSDictionary *> *assets = [NSMutableArray array];
        NSUInteger total = 0;
        BOOL truncated = NO;
        NSUInteger count = fetch.count;

        for (NSUInteger index = 0; index < count; index++) {
            @autoreleasepool {
                PHAsset *asset = [fetch objectAtIndex:index];
                if (media_type == PHAssetMediaTypeImage || media_type == PHAssetMediaTypeVideo) {
                    if (asset.mediaType != (PHAssetMediaType)media_type) {
                        continue;
                    }
                }
                if (favorites_only && !asset.favorite) {
                    continue;
                }
                if (start_ms > 0 || end_ms > 0) {
                    if (asset.creationDate == nil) {
                        continue;
                    }
                    long long date_ms =
                        (long long)llround(asset.creationDate.timeIntervalSince1970 * 1000.0);
                    if (start_ms > 0 && date_ms < start_ms) {
                        continue;
                    }
                    if (end_ms > 0 && date_ms > end_ms) {
                        continue;
                    }
                }
                NSDictionary *item = O2PhotosAssetDictionary(asset);
                unsigned long long size = [item[@"size"] unsignedLongLongValue];
                // Unknown sizes are kept: the caller may still show them.
                if (min_size > 0 && size > 0 && size < min_size) {
                    continue;
                }
                total++;
                if (assets.count >= limit) {
                    // There is at least one more matching asset: no need to
                    // read the rest of a large library just to count it.
                    truncated = YES;
                    break;
                }
                [assets addObject:item];
            }
        }

        return O2PhotosCopyString(O2PhotosJSONString(@{
            @"assets": assets,
            @"total": @(total),
            @"truncated": @(truncated),
        }));
    }
}

char *o2_photos_asset_json(const char *identifier) {
    @autoreleasepool {
        if (!O2PhotosHasAccess()) {
            return O2PhotosJSONError(@"O2 Cloud no tiene permiso para leer Fotos.");
        }
        if (identifier == NULL) {
            return O2PhotosJSONError(@"Falta el identificador del elemento de Fotos.");
        }
        NSString *local_identifier = [NSString stringWithUTF8String:identifier];
        if (local_identifier.length == 0) {
            return O2PhotosJSONError(@"El identificador del elemento de Fotos está vacío.");
        }

        PHFetchResult<PHAsset *> *result =
            [PHAsset fetchAssetsWithLocalIdentifiers:@[local_identifier] options:nil];
        PHAsset *asset = result.firstObject;
        if (asset == nil) {
            return O2PhotosJSONError(@"Ese elemento ya no está en la biblioteca de Fotos.");
        }
        return O2PhotosCopyString(O2PhotosJSONString(O2PhotosAssetDictionary(asset)));
    }
}

/// Export the original file of an asset to `destination`. Returns NULL on
/// success, or a malloc'd error message. Blocking: call it from a worker.
char *o2_photos_export(
    const char *identifier,
    const char *destination,
    void *context,
    O2PhotosProgressCallback progress
) {
    @autoreleasepool {
        if (!O2PhotosHasAccess()) {
            return O2PhotosCopyString(@"O2 Cloud no tiene permiso para leer Fotos.");
        }
        if (identifier == NULL || destination == NULL) {
            return O2PhotosCopyString(@"Faltan datos para exportar el elemento de Fotos.");
        }
        if (@available(macOS 10.15, *)) {
            // PHAssetResourceManager and its request options exist here.
        } else {
            return O2PhotosCopyString(@"Subir desde Fotos requiere macOS 10.15 o posterior.");
        }

        NSString *local_identifier = [NSString stringWithUTF8String:identifier];
        NSString *path = [NSString stringWithUTF8String:destination];
        if (local_identifier.length == 0 || path.length == 0) {
            return O2PhotosCopyString(@"Faltan datos para exportar el elemento de Fotos.");
        }

        PHFetchResult<PHAsset *> *result =
            [PHAsset fetchAssetsWithLocalIdentifiers:@[local_identifier] options:nil];
        PHAsset *asset = result.firstObject;
        if (asset == nil) {
            return O2PhotosCopyString(@"Ese elemento ya no está en la biblioteca de Fotos.");
        }

        PHAssetResource *resource = O2PhotosBestResource(asset);
        if (resource == nil) {
            return O2PhotosCopyString(@"Este elemento no tiene un archivo que se pueda descargar.");
        }

        NSFileManager *file_manager = [NSFileManager defaultManager];
        NSString *parent = [path stringByDeletingLastPathComponent];
        [file_manager createDirectoryAtPath:parent
               withIntermediateDirectories:YES
                                attributes:nil
                                     error:nil];
        [file_manager removeItemAtPath:path error:nil];
        if (![file_manager createFileAtPath:path contents:nil attributes:nil]) {
            return O2PhotosCopyString(@"No se ha podido crear el archivo temporal.");
        }

        NSFileHandle *handle = [NSFileHandle fileHandleForWritingAtPath:path];
        if (handle == nil) {
            [file_manager removeItemAtPath:path error:nil];
            return O2PhotosCopyString(@"No se ha podido abrir el archivo temporal.");
        }

        __block NSError *export_error = nil;
        __block unsigned long long written = 0;
        unsigned long long known_total = O2PhotosResourceSize(resource);
        dispatch_semaphore_t semaphore = dispatch_semaphore_create(0);

        PHAssetResourceRequestOptions *options = [[PHAssetResourceRequestOptions alloc] init];
        options.networkAccessAllowed = YES;

        [[PHAssetResourceManager defaultManager]
            requestDataForAssetResource:resource
                                options:options
                  dataReceivedHandler:^(NSData *data) {
            @synchronized (handle) {
                if (export_error != nil) {
                    return;
                }
                @try {
                    [handle writeData:data];
                    written += data.length;
                } @catch (NSException *exception) {
                    export_error = [NSError
                        errorWithDomain:@"O2CloudPhotos"
                                   code:1
                               userInfo:@{
                                   NSLocalizedDescriptionKey:
                                       exception.reason ?: @"No se ha podido escribir el archivo",
                               }];
                }
            }
            if (progress != NULL) {
                progress(context, written, known_total);
            }
        }
              completionHandler:^(NSError *error) {
            @synchronized (handle) {
                if (error != nil && export_error == nil) {
                    export_error = error;
                }
            }
            dispatch_semaphore_signal(semaphore);
        }];

        dispatch_semaphore_wait(semaphore, DISPATCH_TIME_FOREVER);

        @synchronized (handle) {
            @try {
                [handle closeFile];
            } @catch (NSException *exception) {
                // Nothing useful to do: the file is removed below if needed.
            }
        }

        if (export_error != nil) {
            [file_manager removeItemAtPath:path error:nil];
            return O2PhotosCopyString(
                export_error.localizedDescription ?: @"No se ha podido exportar el elemento de Fotos."
            );
        }
        if (written == 0) {
            [file_manager removeItemAtPath:path error:nil];
            return O2PhotosCopyString(@"Fotos no ha devuelto ningún dato para este elemento.");
        }
        return NULL;
    }
}

void o2_photos_free(char *value) {
    if (value != NULL) {
        free(value);
    }
}
