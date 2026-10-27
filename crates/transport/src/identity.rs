//! The signed-in user and logon session that pipes belong to.

use std::{ffi::c_void, sync::OnceLock};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree},
        Security::{
            Authorization::{ConvertSidToStringSidW, GetSecurityInfo, SE_KERNEL_OBJECT},
            EqualSid, GetLengthSid, GetTokenInformation, OWNER_SECURITY_INFORMATION,
            PSECURITY_DESCRIPTOR, PSID, TOKEN_QUERY, TOKEN_USER, TokenSessionId, TokenUser,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    },
    core::{PWSTR, Result},
};

/// Large enough for `TOKEN_USER` with the longest possible SID.
const TOKEN_USER_BUFFER_WORDS: usize = 16;

pub(crate) struct User {
    /// The binary SID, kept 4-byte aligned for the security APIs.
    sid: Vec<u32>,
    pub sid_string: String,
    pub session: u32,
}

impl User {
    /// The process token's user and session. Elevated and AppContainer
    /// processes of the same user report the same SID.
    pub fn current() -> Result<&'static User> {
        static USER: OnceLock<User> = OnceLock::new();
        if let Some(user) = USER.get() {
            return Ok(user);
        }
        let user = Self::query()?;
        Ok(USER.get_or_init(|| user))
    }

    fn query() -> Result<User> {
        unsafe {
            let mut token = HANDLE::default();
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
            let result = Self::from_token(token);
            let _ = CloseHandle(token);
            result
        }
    }

    unsafe fn from_token(token: HANDLE) -> Result<User> {
        unsafe {
            let mut buffer = [0u64; TOKEN_USER_BUFFER_WORDS];
            let mut length = 0;
            GetTokenInformation(
                token,
                TokenUser,
                Some(buffer.as_mut_ptr().cast()),
                size_of_val(&buffer) as u32,
                &mut length,
            )?;
            let sid = (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid;
            let sid_length = GetLengthSid(sid) as usize;
            let mut owned = vec![0u32; sid_length.div_ceil(4)];
            std::ptr::copy_nonoverlapping(
                sid.0.cast::<u8>(),
                owned.as_mut_ptr().cast(),
                sid_length,
            );
            let mut text = PWSTR::null();
            ConvertSidToStringSidW(sid, &mut text)?;
            let sid_string = text.to_string();
            let _ = LocalFree(Some(HLOCAL(text.0.cast())));
            let mut session = 0u32;
            GetTokenInformation(
                token,
                TokenSessionId,
                Some((&raw mut session).cast::<c_void>()),
                size_of::<u32>() as u32,
                &mut length,
            )?;
            Ok(User {
                sid: owned,
                sid_string: sid_string?,
                session,
            })
        }
    }

    fn psid(&self) -> PSID {
        PSID(self.sid.as_ptr().cast_mut().cast())
    }

    /// Whether this user owns the kernel object behind `handle`.
    pub fn owns(&self, handle: HANDLE) -> Result<bool> {
        unsafe {
            let mut owner = PSID::default();
            let mut descriptor = PSECURITY_DESCRIPTOR::default();
            GetSecurityInfo(
                handle,
                SE_KERNEL_OBJECT,
                OWNER_SECURITY_INFORMATION,
                Some(&mut owner),
                None,
                None,
                None,
                Some(&mut descriptor),
            )
            .ok()?;
            let owned = EqualSid(owner, self.psid()).is_ok();
            let _ = LocalFree(Some(HLOCAL(descriptor.0)));
            Ok(owned)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_process_user() {
        let user = User::current().unwrap();
        assert!(user.sid_string.starts_with("S-1-5-"));
        assert!(std::ptr::eq(user, User::current().unwrap()));
    }
}
