#import <Foundation/Foundation.h>
#import <LocalAuthentication/LocalAuthentication.h>
#import <Security/Security.h>
#import <dispatch/dispatch.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

// Errors use caller-owned buffers; sync JSON is released with uwlt_vault_free.
enum { UWLT_OK = 0, UWLT_ERROR = 1, UWLT_CANCELLED = 2 };

static int fail(char *error, size_t capacity, NSString *message, BOOL cancelled) {
    if (error != NULL && capacity != 0) {
        snprintf(error, capacity, "%s", message.UTF8String ?: "Keychain operation failed");
    }
    return cancelled ? UWLT_CANCELLED : UWLT_ERROR;
}

static int authError(char *error, size_t capacity, NSError *cause) {
    BOOL cancelled = [cause.domain isEqualToString:LAErrorDomain] &&
        (cause.code == LAErrorUserCancel || cause.code == LAErrorAppCancel ||
         cause.code == LAErrorSystemCancel || cause.code == LAErrorUserFallback);
    return fail(error, capacity,
                cancelled ? @"Touch ID authorization cancelled; no operation performed"
                          : [NSString stringWithFormat:@"Touch ID authorization failed: %@",
                             cause.localizedDescription ?: @"biometric authentication unavailable"],
                cancelled);
}

static int keychainError(char *error, size_t capacity, OSStatus status) {
    if (status == errSecUserCanceled) {
        return fail(error, capacity, @"Touch ID authorization cancelled; no operation performed", YES);
    }
    NSString *detail = CFBridgingRelease(SecCopyErrorMessageString(status, NULL));
    NSString *hint = status == errSecMissingEntitlement
        ? @"; run the signed, provisioned app-bundle executable in your logged-in user session" : @"";
    return fail(error, capacity,
                [NSString stringWithFormat:@"Keychain error %d: %@%@", (int)status,
                 detail ?: @"unknown error", hint], NO);
}

static LAContext *newContext(NSString *reason) {
    LAContext *context = [[LAContext alloc] init];
    context.touchIDAuthenticationAllowableReuseDuration = 0;
    context.localizedFallbackTitle = @"";
    context.localizedCancelTitle = @"Cancel";
    context.localizedReason = reason;
    return context;
}

static int checkBiometry(LAContext *context, char *error, size_t capacity) {
    NSError *cause = nil;
    if (![context canEvaluatePolicy:LAPolicyDeviceOwnerAuthenticationWithBiometrics error:&cause]) {
        return authError(error, capacity, cause);
    }
    if (context.biometryType != LABiometryTypeTouchID) {
        return fail(error, capacity, @"Touch ID is required; no password or other fallback is permitted", NO);
    }
    return UWLT_OK;
}

static int authorize(LAContext *context, NSString *reason, char *error, size_t capacity) {
    int result = checkBiometry(context, error, capacity);
    if (result != UWLT_OK) return result;
    dispatch_semaphore_t finished = dispatch_semaphore_create(0);
    __block BOOL authenticated = NO;
    __block NSError *cause = nil;
    [context evaluatePolicy:LAPolicyDeviceOwnerAuthenticationWithBiometrics
            localizedReason:reason reply:^(BOOL success, NSError *failure) {
        authenticated = success;
        cause = failure;
        dispatch_semaphore_signal(finished);
    }];
    dispatch_semaphore_wait(finished, DISPATCH_TIME_FOREVER);
    if (!authenticated) return authError(error, capacity, cause);
    // Never permit SecItem to start a second authentication flow after this one.
    context.interactionNotAllowed = YES;
    return UWLT_OK;
}

static NSMutableDictionary *query(NSString *account, LAContext *context, id synchronizable) {
    NSMutableDictionary *item = [@{
        (__bridge id)kSecClass: (__bridge id)kSecClassGenericPassword,
        (__bridge id)kSecAttrService: @"org.uwlt.wallet",
        (__bridge id)kSecUseDataProtectionKeychain: @YES,
        (__bridge id)kSecAttrSynchronizable: synchronizable,
        (__bridge id)kSecUseAuthenticationContext: context,
    } mutableCopy];
    if (account != nil) item[(__bridge id)kSecAttrAccount] = account;
    return item;
}

static NSData *addressMetadata(NSString *address, NSString *solanaAddress) {
    NSMutableDictionary *metadata = [@{@"address": address} mutableCopy];
    if ([solanaAddress isKindOfClass:[NSString class]] && solanaAddress.length != 0) {
        metadata[@"solana_address"] = solanaAddress;
    }
    return [NSJSONSerialization dataWithJSONObject:metadata options:0 error:NULL];
}

static NSMutableDictionary *syncItem(NSString *account, NSString *label, NSString *address,
                                    NSString *solanaAddress, LAContext *context, NSData *key) {
    NSMutableDictionary *item = query(account, context, @YES);
    // Synchronizable items cannot use biometryCurrentSet; uwlt authorizes explicitly.
    item[(__bridge id)kSecAttrAccessible] = (__bridge id)kSecAttrAccessibleWhenUnlocked;
    item[(__bridge id)kSecAttrLabel] = label;
    item[(__bridge id)kSecAttrGeneric] = addressMetadata(address, solanaAddress);
    item[(__bridge id)kSecAttrComment] = @"TRON / EVM / Solana private key";
    item[(__bridge id)kSecValueData] = key;
    return item;
}

static BOOL arguments(const char *id, const char *reason, NSString **account,
                      NSString **prompt, char *error, size_t capacity) {
    *account = id == NULL ? nil : [NSString stringWithUTF8String:id];
    *prompt = reason == NULL ? nil : [NSString stringWithUTF8String:reason];
    if ((*account).length == 0 || (*prompt).length == 0) {
        fail(error, capacity, @"A nonempty UTF-8 wallet ID and authorization reason are required", NO);
        return NO;
    }
    return YES;
}

int uwlt_vault_available(char *error, size_t capacity) {
    @autoreleasepool {
        LAContext *context = newContext(@"");
        int result = checkBiometry(context, error, capacity);
        [context invalidate];
        return result;
    }
}

int uwlt_vault_save(const char *identifier, const char *label, const char *address,
                   const char *solanaAddress, const uint8_t *key, const char *reason,
                   char *error, size_t capacity) {
    @autoreleasepool {
        NSString *account = nil, *prompt = nil;
        if (!arguments(identifier, reason, &account, &prompt, error, capacity)) return UWLT_ERROR;
        if (key == NULL) return fail(error, capacity, @"Missing private key buffer", NO);
        LAContext *context = newContext(prompt);
        @try {
            int result = authorize(context, prompt, error, capacity);
            if (result != UWLT_OK) return result;
            // Borrow Rust's zeroizing buffer; do not allocate a second plaintext copy.
            NSData *data = [NSData dataWithBytesNoCopy:(void *)key length:64 freeWhenDone:NO];
            NSMutableDictionary *item = syncItem(
                account, [NSString stringWithUTF8String:label],
                [NSString stringWithUTF8String:address],
                [NSString stringWithUTF8String:solanaAddress], context, data);
            OSStatus status = SecItemAdd((__bridge CFDictionaryRef)item, NULL);
            // Never update an existing account implicitly.
            return status == errSecSuccess ? UWLT_OK : keychainError(error, capacity, status);
        } @finally {
            [context invalidate];
        }
    }
}

int uwlt_vault_load(const char *identifier, uint8_t *key, const char *reason,
                   char *error, size_t capacity) {
    @autoreleasepool {
        if (key == NULL) return fail(error, capacity, @"Missing private key output buffer", NO);
        NSString *account = nil, *prompt = nil;
        if (!arguments(identifier, reason, &account, &prompt, error, capacity)) return UWLT_ERROR;
        LAContext *context = newContext(prompt);
        CFTypeRef data = NULL;
        @try {
            int result = authorize(context, prompt, error, capacity);
            if (result != UWLT_OK) return result;
            NSMutableDictionary *item = query(account, context,
                                              (__bridge id)kSecAttrSynchronizableAny);
            item[(__bridge id)kSecReturnData] = @YES;
            item[(__bridge id)kSecMatchLimit] = (__bridge id)kSecMatchLimitOne;
            OSStatus status = SecItemCopyMatching((__bridge CFDictionaryRef)item, &data);
            if (status != errSecSuccess) return keychainError(error, capacity, status);
            if (data == NULL || CFGetTypeID(data) != CFDataGetTypeID() ||
                CFDataGetLength((CFDataRef)data) != 64) {
                return fail(error, capacity, @"Keychain item is not a hexadecimal private key", NO);
            }
            CFDataGetBytes((CFDataRef)data, CFRangeMake(0, 64), key);
            return UWLT_OK;
        } @finally {
            // Security owns immutable CFData storage: release immediately, never cast
            // it to writable memory. Only the caller-owned Zeroizing buffer escapes.
            if (data != NULL) CFRelease(data);
            [context invalidate];
        }
    }
}

int uwlt_vault_delete(const char *identifier, const char *reason, char *error, size_t capacity) {
    @autoreleasepool {
        NSString *account = nil, *prompt = nil;
        if (!arguments(identifier, reason, &account, &prompt, error, capacity)) return UWLT_ERROR;
        LAContext *context = newContext(prompt);
        @try {
            // Deletion itself does not enforce a keychain item's read ACL.
            int result = authorize(context, prompt, error, capacity);
            if (result != UWLT_OK) return result;
            OSStatus status = SecItemDelete((__bridge CFDictionaryRef)query(
                account, context, (__bridge id)kSecAttrSynchronizableAny));
            return status == errSecSuccess ? UWLT_OK : keychainError(error, capacity, status);
        } @finally {
            [context invalidate];
        }
    }
}

static int migrateWallet(NSDictionary *wallet, LAContext *context, char *error, size_t capacity) {
    NSString *account = wallet[@"id"];
    NSMutableDictionary *legacy = query(account, context, @NO);
    NSMutableDictionary *readLegacy = [legacy mutableCopy];
    readLegacy[(__bridge id)kSecReturnData] = @YES;
    readLegacy[(__bridge id)kSecMatchLimit] = (__bridge id)kSecMatchLimitOne;
    CFTypeRef source = NULL, target = NULL;
    @try {
        OSStatus status = SecItemCopyMatching((__bridge CFDictionaryRef)readLegacy, &source);
        if (status == errSecItemNotFound) return UWLT_OK;
        if (status != errSecSuccess) return keychainError(error, capacity, status);
        if (source == NULL || CFGetTypeID(source) != CFDataGetTypeID() ||
            CFDataGetLength((CFDataRef)source) != 64) {
            return fail(error, capacity, @"Legacy Keychain item is not a hexadecimal private key", NO);
        }
        NSString *label = [@"uwlt: " stringByAppendingString:wallet[@"name"]];
        NSMutableDictionary *item = syncItem(account, label, wallet[@"address"],
                                            wallet[@"solana_address"], context, (__bridge NSData *)source);
        status = SecItemAdd((__bridge CFDictionaryRef)item, NULL);
        if (status == errSecDuplicateItem) {
            NSMutableDictionary *existing = query(account, context, @YES);
            existing[(__bridge id)kSecReturnData] = @YES;
            existing[(__bridge id)kSecMatchLimit] = (__bridge id)kSecMatchLimitOne;
            status = SecItemCopyMatching((__bridge CFDictionaryRef)existing, &target);
            if (status != errSecSuccess) return keychainError(error, capacity, status);
            if (target == NULL || CFGetTypeID(target) != CFDataGetTypeID() ||
                !CFEqual(source, target)) {
                return fail(error, capacity,
                            @"A different synchronized key uses this wallet ID; legacy key preserved", NO);
            }
        } else if (status != errSecSuccess) {
            return keychainError(error, capacity, status);
        }
        // Keep the source until the cloud item exists with the same secret.
        status = SecItemDelete((__bridge CFDictionaryRef)legacy);
        return status == errSecSuccess || status == errSecItemNotFound
            ? UWLT_OK : keychainError(error, capacity, status);
    } @finally {
        if (target != NULL) CFRelease(target);
        if (source != NULL) CFRelease(source);
    }
}

typedef int (*DeriveSolana)(const uint8_t *, const char *, char *, size_t);

static NSString *migrateSolanaAddress(NSString *account, NSString *address, LAContext *context,
                                     DeriveSolana derive, char *error, size_t capacity) {
    CFTypeRef data = NULL;
    @try {
        NSMutableDictionary *item = query(account, context, @YES);
        item[(__bridge id)kSecReturnData] = @YES;
        item[(__bridge id)kSecMatchLimit] = (__bridge id)kSecMatchLimitOne;
        OSStatus status = SecItemCopyMatching((__bridge CFDictionaryRef)item, &data);
        if (status != errSecSuccess) {
            keychainError(error, capacity, status);
            return nil;
        }
        char solana[45] = {0};
        if (data == NULL || CFGetTypeID(data) != CFDataGetTypeID() ||
            CFDataGetLength((CFDataRef)data) != 64 ||
            derive(CFDataGetBytePtr((CFDataRef)data), address.UTF8String,
                   solana, sizeof(solana)) != 0) {
            fail(error, capacity, @"Cannot derive Solana address: invalid key or wallet address mismatch", NO);
            return nil;
        }
        NSString *result = [NSString stringWithUTF8String:solana];
        NSDictionary *update = @{
            (__bridge id)kSecAttrGeneric: addressMetadata(address, result),
            (__bridge id)kSecAttrComment: @"TRON / EVM / Solana private key",
        };
        status = SecItemUpdate((__bridge CFDictionaryRef)query(account, context, @YES),
                               (__bridge CFDictionaryRef)update);
        if (status != errSecSuccess) {
            keychainError(error, capacity, status);
            return nil;
        }
        return result;
    } @finally {
        if (data != NULL) CFRelease(data);
    }
}

int uwlt_vault_sync(const uint8_t *wallets, size_t walletsLength, const char *reason,
                   DeriveSolana deriveSolana, uint8_t **json, size_t *jsonLength,
                   char *error, size_t capacity) {
    @autoreleasepool {
        *json = NULL;
        *jsonLength = 0;
        NSData *input = [NSData dataWithBytesNoCopy:(void *)wallets
                                           length:walletsLength freeWhenDone:NO];
        NSError *cause = nil;
        NSArray *metadata = [NSJSONSerialization JSONObjectWithData:input options:0 error:&cause];
        if (metadata == nil) return fail(error, capacity, cause.localizedDescription, NO);
        NSString *prompt = [NSString stringWithUTF8String:reason];
        LAContext *context = newContext(prompt);
        CFTypeRef attributes = NULL;
        @try {
            int result = authorize(context, prompt, error, capacity);
            if (result != UWLT_OK) return result;
            for (NSDictionary *wallet in metadata) {
                result = migrateWallet(wallet, context, error, capacity);
                if (result != UWLT_OK) return result;
            }
            NSMutableDictionary *items = query(nil, context, @YES);
            items[(__bridge id)kSecReturnAttributes] = @YES;
            items[(__bridge id)kSecMatchLimit] = (__bridge id)kSecMatchLimitAll;
            OSStatus status = SecItemCopyMatching((__bridge CFDictionaryRef)items, &attributes);
            if (status != errSecSuccess && status != errSecItemNotFound) {
                return keychainError(error, capacity, status);
            }
            NSMutableArray *discovered = [NSMutableArray array];
            if (status == errSecSuccess) {
                for (NSDictionary *item in (__bridge NSArray *)attributes) {
                    NSString *account = item[(__bridge id)kSecAttrAccount];
                    NSString *label = item[(__bridge id)kSecAttrLabel];
                    NSData *generic = item[(__bridge id)kSecAttrGeneric];
                    id decoded = generic == nil ? nil :
                        [NSJSONSerialization JSONObjectWithData:generic options:0 error:NULL];
                    NSString *address = nil, *solanaAddress = nil;
                    if ([decoded isKindOfClass:[NSDictionary class]]) {
                        address = decoded[@"address"];
                        solanaAddress = decoded[@"solana_address"];
                    } else if (generic != nil && decoded == nil) {
                        // Before Solana support the attribute contained a bare TRON address.
                        address = [[NSString alloc] initWithData:generic encoding:NSUTF8StringEncoding];
                    }
                    if (account.length == 0 || ![label hasPrefix:@"uwlt: "] ||
                        ![address isKindOfClass:[NSString class]] || address.length == 0) {
                        return fail(error, capacity,
                                    @"Synchronized wallet is missing its name, ID, or address", NO);
                    }
                    if (![solanaAddress isKindOfClass:[NSString class]] || solanaAddress.length == 0) {
                        solanaAddress = migrateSolanaAddress(account, address, context,
                                                             deriveSolana, error, capacity);
                        if (solanaAddress == nil) return UWLT_ERROR;
                    }
                    [discovered addObject:@{
                        @"id": account,
                        @"name": [label substringFromIndex:6],
                        @"address": address,
                        @"solana_address": solanaAddress,
                    }];
                }
            }
            NSData *encoded = [NSJSONSerialization dataWithJSONObject:discovered options:0 error:&cause];
            if (encoded == nil) return fail(error, capacity, cause.localizedDescription, NO);
            uint8_t *output = malloc(encoded.length);
            if (output == NULL) return fail(error, capacity, @"Cannot allocate wallet metadata JSON", NO);
            memcpy(output, encoded.bytes, encoded.length);
            *json = output;
            *jsonLength = encoded.length;
            return UWLT_OK;
        } @finally {
            if (attributes != NULL) CFRelease(attributes);
            [context invalidate];
        }
    }
}

void uwlt_vault_free(uint8_t *json) {
    free(json);
}
