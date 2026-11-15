//! The existing file-based login keychain item used by Antigravity.
use super::{decode_keyring, encode_keyring, err};
use crate::{errors::code, storage::Result};
use objc2_core_foundation::{CFBoolean, CFData, CFDictionary, CFRetained, CFString, CFType};
use objc2_security::{
    errSecAuthFailed as ERR_SEC_AUTH_FAILED,
    errSecInteractionNotAllowed as ERR_SEC_INTERACTION_NOT_ALLOWED,
    errSecItemNotFound as ERR_SEC_ITEM_NOT_FOUND, errSecSuccess as ERR_SEC_SUCCESS,
    kSecAttrAccount, kSecAttrService, kSecClass, kSecClassGenericPassword, kSecMatchLimit,
    kSecMatchLimitOne, kSecReturnData, kSecValueData, SecItemAdd, SecItemCopyMatching,
    SecItemDelete, SecItemUpdate,
};
use parking_lot::Mutex;
use std::ptr::NonNull;

// The legacy keychain's interaction flag is process-wide. All Vela accesses
// share this lock, including usage attribution outside the account lock.
static ACCESS: Mutex<()> = Mutex::new(());

#[allow(deprecated)] // Required by the existing file keychain, not a new DP keychain.
fn access<T>(interactive: bool, f: impl FnOnce() -> Result<T>) -> Result<T> {
    use objc2_security::SecKeychain;
    let _lock = ACCESS.lock();
    let fail = || err(code::AUTH, "无法访问 macOS 钥匙串中的 Antigravity 登录凭据");
    let mut previous = 0;
    // SAFETY: The output points to a live Boolean and accesses are serialized.
    unsafe {
        if SecKeychain::user_interaction_allowed(NonNull::from(&mut previous)) != ERR_SEC_SUCCESS
            || SecKeychain::set_user_interaction_allowed(interactive) != ERR_SEC_SUCCESS
        {
            return Err(fail());
        }
    }
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            // SAFETY: ACCESS is held until after this guard is dropped.
            unsafe {
                SecKeychain::set_user_interaction_allowed(self.0);
            }
        }
    }
    let _restore = Restore(previous != 0);
    f()
}

fn query(extra: &[(&CFString, &CFType)]) -> CFRetained<CFDictionary<CFString, CFType>> {
    let service = CFString::from_str("gemini");
    let account = CFString::from_str("antigravity");
    // SAFETY: Security.framework's constants are immutable, valid CF objects.
    let mut keys = unsafe { vec![kSecClass, kSecAttrService, kSecAttrAccount] };
    let mut values: Vec<&CFType> = unsafe {
        vec![
            kSecClassGenericPassword.as_ref(),
            service.as_ref(),
            account.as_ref(),
        ]
    };
    for &(key, value) in extra {
        keys.push(key);
        values.push(value);
    }
    CFDictionary::from_slices(&keys, &values)
}

pub(super) fn read(interactive: bool) -> Result<Option<String>> {
    access(interactive, || {
        let fail = || err(code::AUTH, "无法读取 macOS 钥匙串中的 Antigravity 登录凭据");
        // SAFETY: Query requests one generic-password data value. The returned
        // Copy reference is owned here and checked before reading its bytes.
        unsafe {
            let query = query(&[
                (kSecReturnData, CFBoolean::new(true).as_ref()),
                (kSecMatchLimit, kSecMatchLimitOne.as_ref()),
            ]);
            let mut value = std::ptr::null();
            match SecItemCopyMatching(query.as_opaque(), &mut value) {
                ERR_SEC_ITEM_NOT_FOUND => Ok(None),
                ERR_SEC_SUCCESS => {
                    let value =
                        CFRetained::from_raw(NonNull::new(value.cast_mut()).ok_or_else(fail)?);
                    let data = value.downcast::<CFData>().map_err(|_| fail())?;
                    Ok(std::str::from_utf8(&data.to_vec())
                        .ok()
                        .and_then(decode_keyring))
                }
                ERR_SEC_INTERACTION_NOT_ALLOWED | ERR_SEC_AUTH_FAILED => Err(err(
                    code::AUTH,
                    "macOS 钥匙串尚未授权或已锁定，请在 Antigravity 账号页刷新授权",
                )),
                status => Err(err(
                    code::AUTH,
                    &format!("无法读取 macOS 钥匙串中的 Antigravity 登录凭据（系统错误 {status}）"),
                )),
            }
        }
    })
}

pub(super) fn write(raw: Option<&str>) -> Result<()> {
    access(true, || {
        // SAFETY: Dictionaries contain the documented generic-password keys
        // and data. Update preserves the existing item's access controls.
        let status = unsafe {
            let item = query(&[]);
            match raw {
                Some(raw) => {
                    let data = CFData::from_bytes(encode_keyring(raw).as_bytes());
                    let update =
                        CFDictionary::from_slices(&[kSecValueData], &[data.as_ref() as &CFType]);
                    match SecItemUpdate(item.as_opaque(), update.as_opaque()) {
                        ERR_SEC_ITEM_NOT_FOUND => SecItemAdd(
                            query(&[(kSecValueData, data.as_ref())]).as_opaque(),
                            std::ptr::null_mut(),
                        ),
                        status => status,
                    }
                }
                None => match SecItemDelete(item.as_opaque()) {
                    ERR_SEC_ITEM_NOT_FOUND => ERR_SEC_SUCCESS,
                    status => status,
                },
            }
        };
        if status == ERR_SEC_SUCCESS {
            Ok(())
        } else {
            Err(err(
                code::AUTH,
                "无法写入 macOS 钥匙串中的 Antigravity 登录凭据",
            ))
        }
    })
}
