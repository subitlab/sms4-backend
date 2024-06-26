//! Account system.

use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
};

use lettre::{transport::smtp, AsyncSmtpTransport};
use libaccount::{Academy, House};
use serde::{Deserialize, Serialize};

use crate::{config, Error, TestCx};

use self::verify::{Captcha, VerifyCx, VerifyVariant};

pub mod verify;

/// A permission group of an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[non_exhaustive]
pub enum Permission {
    /// Post postings.
    ///
    /// # Containing permissions
    ///
    /// - [`Self::GetPubPost`]
    Post,
    /// Get public posts.
    GetPubPost,
    /// View, approve or reject posts.
    ///
    /// # Containing permissions
    ///
    /// - [`Self::GetPubPost`]
    ReviewPost,
    /// Remove posts.
    ///
    /// # Containing permissions
    ///
    /// - [`Self::GetPubPost`]
    /// - [`Self::ReviewPost`]
    RemovePost,

    /// Appends or removes permissions from
    /// an account.
    ///
    /// # Containing permissions
    ///
    /// - [`Self::ViewSimpleAccount`]
    /// - [`Self::ViewFullAccount`]
    SetPermissions,
    /// Gets full information of an account.
    ///
    /// # Containing permissions
    ///
    /// - [`Self::ViewSimpleAccount`]
    ViewFullAccount,
    /// Gets simple information of an account.
    ViewSimpleAccount,

    /// Manage notifications.
    ///
    /// # Containing permissions
    ///
    /// - [`Self::GetPubNotifications`]
    ManageNotifications,
    /// Get public notifications.
    GetPubNotifications,

    /// Upload resources.
    UploadResource,

    /// Maintain this system.
    Maintain,
}

impl libaccount::Permission for Permission {
    #[inline]
    fn default_set() -> std::collections::HashSet<Self> {
        [
            Self::Post,
            Self::GetPubPost,
            Self::ViewSimpleAccount,
            Self::UploadResource,
            Self::GetPubNotifications,
        ]
        .into()
    }

    #[inline]
    fn contains(&self, permission: &Self) -> bool {
        matches!(
            (self, permission),
            (Permission::Post, Permission::GetPubPost)
                | (
                    Permission::SetPermissions,
                    Permission::ViewFullAccount | Permission::ViewSimpleAccount
                )
                | (Permission::ViewFullAccount, Permission::ViewSimpleAccount)
                | (Permission::ReviewPost, Permission::GetPubPost)
                | (
                    Permission::RemovePost,
                    Permission::GetPubPost | Permission::ReviewPost
                )
                | (
                    Permission::ManageNotifications,
                    Permission::GetPubNotifications
                )
        )
    }
}

/// A tag of an account.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Hash, Clone)]
#[serde(tag = "entry", content = "tag")]
pub enum Tag {
    /// A permission group.
    Permission(Permission),
    /// A department.
    Department(String),
    /// A house.
    House(House),
    /// An academy.
    Academy(Academy),
}

/// The entry of a [`Tag`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TagEntry {
    /// A permission group.
    Permission,
    /// A department.
    Department,
    /// A house.
    House,
    /// An academy.
    Academy,
}

impl libaccount::tag::Tag for Tag {
    type Entry = TagEntry;

    #[inline]
    fn as_entry(&self) -> Self::Entry {
        match self {
            Tag::Permission(_) => TagEntry::Permission,
            Tag::Department(_) => TagEntry::Department,
            Tag::House(_) => TagEntry::House,
            Tag::Academy(_) => TagEntry::Academy,
        }
    }
}

impl libaccount::tag::AsPermission for Tag {
    type Permission = Permission;

    #[inline]
    fn as_permission(&self) -> Option<&<Tag as libaccount::tag::AsPermission>::Permission> {
        if let Self::Permission(p) = self {
            Some(p)
        } else {
            None
        }
    }
}

impl From<Permission> for Tag {
    #[inline]
    fn from(value: Permission) -> Self {
        Self::Permission(value)
    }
}

impl libaccount::tag::PermissionEntry for TagEntry {
    const VALUE: Self = Self::Permission;
}

impl libaccount::tag::UserDefinableEntry for TagEntry {
    #[inline]
    fn is_user_defineable(&self) -> bool {
        !matches!(self, TagEntry::Permission)
    }
}

/// The external data of a verified account.\
/// Containing verify sessions.
#[derive(Serialize, Deserialize, Debug, Default)]
pub struct Ext {
    /// Verify sessions.
    verifies: HashMap<VerifyVariant, VerifyCx>,
}

/// A verified account.
///
/// # Verify Sessions
///
/// Sessions that requires email verifying, like reseting password,
/// are verify sessions. (See [`VerifyVariant`])
/// Verify sessions are stored in external data as [`Ext`].
///
/// Currently, the only verify session is reset password.
#[derive(Debug)]
pub struct Account {
    /// The inner account.
    inner: libaccount::Account<Tag, Ext>,
}

impl Account {
    /// Requests to reset password and sends an email to user.
    ///
    /// # Errors
    ///
    /// - Errors if the difference between the last request time
    /// and the current time is no more than 10 minutes.
    /// - Errors if the email send failed.
    #[inline]
    pub async fn req_reset_password<E>(
        &mut self,
        config: &config::SMTP,
        transport: &AsyncSmtpTransport<E>,
        cx: &TestCx,
    ) -> Result<(), Error>
    where
        E: lettre::Executor,
        AsyncSmtpTransport<E>: lettre::AsyncTransport<Error = smtp::Error>,
    {
        self.req_verify(VerifyVariant::ResetPassword, config, transport, cx)
            .await
    }

    /// Resets the password with given new password.
    ///
    /// # Errors
    ///
    /// - Errors if the captcha is incorrect.
    #[inline]
    pub fn reset_password<T>(&mut self, captcha: Captcha, new_password: T) -> Result<(), Error>
    where
        T: AsRef<str>,
    {
        self.do_verify(VerifyVariant::ResetPassword, captcha)?;
        self.inner.set_password(new_password);
        Ok(())
    }

    /// Requests a verify session and sends an email to user.
    ///
    /// # Errors
    ///
    /// - Errors if the difference between the last request time
    /// and the current time is no more than 10 minutes.
    /// - Errors if the email send failed.
    async fn req_verify<E>(
        &mut self,
        variant: VerifyVariant,
        config: &config::SMTP,
        transport: &AsyncSmtpTransport<E>,
        cx: &TestCx,
    ) -> Result<(), Error>
    where
        E: lettre::Executor,
        AsyncSmtpTransport<E>: lettre::AsyncTransport<Error = smtp::Error>,
    {
        let to = self.inner.email().parse()?;
        let ext = self.inner.ext_mut();
        if let Some(cx) = ext.verifies.get_mut(&variant) {
            cx.update()?;
        } else {
            ext.verifies.insert(variant, VerifyCx::new());
        }
        ext.verifies
            .get_mut(&variant)
            .unwrap()
            .send_email(config, to, variant, transport, cx)
            .await
    }

    /// Validates the verify session captcha and removes the session entry
    /// if the captcha is correct, or throw an error.
    fn do_verify(&mut self, variant: VerifyVariant, captcha: Captcha) -> Result<(), Error> {
        if self
            .inner
            .ext()
            .verifies
            .get(&variant)
            .ok_or(Error::VerifySessionNotFound(variant))?
            .captcha()
            == captcha
        {
            self.inner.ext_mut().verifies.remove(&variant);
            Ok(())
        } else {
            Err(Error::CaptchaIncorrect)
        }
    }
}

impl From<libaccount::Account<Tag, Ext>> for Account {
    #[inline]
    fn from(inner: libaccount::Account<Tag, Ext>) -> Self {
        Self { inner }
    }
}

impl dmds::Data for Account {
    const DIMS: usize = 1;
    const VERSION: u32 = 1;

    #[inline]
    fn dim(&self, dim: usize) -> u64 {
        match dim {
            0 => self.id(),
            _ => unreachable!(),
        }
    }

    fn decode<B: bytes::Buf>(version: u32, dims: &[u64], buf: B) -> std::io::Result<Self> {
        match version {
            1 => {
                let mut inner: libaccount::Account<Tag, Ext> =
                    bincode::deserialize_from(buf.reader())
                        .map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err))?;
                unsafe { inner.initialize_id(dims[0]) };
                Ok(Self { inner })
            }
            _ => unreachable!("unsupported data version {version}"),
        }
    }

    #[inline]
    fn encode<B: bytes::BufMut>(&self, buf: B) -> std::io::Result<()> {
        bincode::serialize_into(buf.writer(), &self.inner)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err))
    }
}

impl Deref for Account {
    type Target = libaccount::Account<Tag, Ext>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for Account {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// An unverified account.
#[derive(Debug)]
pub struct Unverified {
    /// The inner unverified account.
    inner: libaccount::Unverified<VerifyCx>,
}

impl Unverified {
    /// Creates a new unverified account.
    ///
    /// # Errors
    ///
    /// - Errors if email is not ended with `@pkuschool.edu.cn`
    /// or `@i.pkuschool.edu.cn`.
    #[inline]
    pub fn new(email: String) -> Result<Self, Error> {
        Ok(Self {
            inner: libaccount::Unverified::new(
                email,
                VerifyCx::new(),
                siphasher::sip::SipHasher24::new(),
            )?,
        })
    }

    /// Requests to send a captcha with given configuration and `transport`.
    ///
    /// # Errors
    ///
    /// - Errors if the difference between the last request time
    /// and the current time is no more than 10 minutes.
    /// - Errors if the email send failed.
    pub async fn send_captcha<E>(
        &mut self,
        config: &config::SMTP,
        transport: &AsyncSmtpTransport<E>,
        cx: &TestCx,
    ) -> Result<(), Error>
    where
        E: lettre::Executor,
        AsyncSmtpTransport<E>: lettre::AsyncTransport<Error = smtp::Error>,
    {
        let to = self.inner.email().parse()?;
        self.inner
            .ext_mut()
            .send_email(config, to, "account activation", transport, cx)
            .await
    }
}

impl dmds::Data for Unverified {
    const DIMS: usize = 1;
    const VERSION: u32 = 1;

    #[inline]
    fn dim(&self, dim: usize) -> u64 {
        match dim {
            0 => self.email_hash(),
            _ => unreachable!(),
        }
    }

    fn decode<B: bytes::Buf>(version: u32, dims: &[u64], buf: B) -> std::io::Result<Self> {
        match version {
            1 => {
                let mut inner: libaccount::Unverified<VerifyCx> =
                    bincode::deserialize_from(buf.reader())
                        .map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err))?;
                unsafe { inner.initialize_email_hash(dims[0]) };
                Ok(Self { inner })
            }
            _ => unreachable!("unsupported data version {version}"),
        }
    }

    #[inline]
    fn encode<B: bytes::BufMut>(&self, buf: B) -> std::io::Result<()> {
        bincode::serialize_into(buf.writer(), &self.inner)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err))
    }
}

impl Deref for Unverified {
    type Target = libaccount::Unverified<VerifyCx>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for Unverified {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl From<Unverified> for libaccount::Unverified<VerifyCx> {
    #[inline]
    fn from(val: Unverified) -> Self {
        val.inner
    }
}

impl From<libaccount::Unverified<VerifyCx>> for Unverified {
    #[inline]
    fn from(value: libaccount::Unverified<VerifyCx>) -> Self {
        Self { inner: value }
    }
}
