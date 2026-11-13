//! OPAQUE (RFC 9807) password authentication, with Argon2id as the key-stretching function.
//!
//! The server stores a password file it cannot use to brute-force the password offline, and the
//! client gets an `export_key` the server never sees, from which it derives the master KEK.
//!
//! Each step takes and returns serialized messages (`Vec<u8>`) so callers can put them on the
//! wire as-is. Both sides must use the same [`OpaqueIdentity`] and [`KdfParams`] in every step.

use opaque_ke::argon2::{Algorithm, Argon2, Params, Version};
use opaque_ke::ciphersuite::CipherSuite;
use opaque_ke::rand::rngs::OsRng;
use opaque_ke::{
    ClientLogin, ClientLoginFinishParameters, ClientRegistration,
    ClientRegistrationFinishParameters, CredentialResponse, Identifiers, RegistrationResponse,
    Ristretto255, TripleDh,
};
use zeroize::Zeroizing;

use crate::kdf::{KdfParams, master_kek};
use crate::secret::SymmetricKey;
use crate::{CryptoError, Result};

/// The OPAQUE cipher suite: ristretto255 OPRF, 3DH key exchange with SHA-512, Argon2id KSF.
pub struct Suite;

impl CipherSuite for Suite {
    type OprfCs = Ristretto255;
    type KeyExchange = TripleDh<Ristretto255, sha2_010::Sha512>;
    type Ksf = Argon2<'static>;
}

/// Identities bound into the OPAQUE transcript. Binding the server identity means a password
/// file registered on one server cannot be replayed against another.
#[derive(Clone, Copy, Debug)]
pub struct OpaqueIdentity<'a> {
    /// Stable user identifier, e.g. the normalized e-mail address.
    pub user: &'a [u8],
    /// Stable server identifier, e.g. the public host name of the agent.
    pub server: &'a [u8],
}

impl<'a> OpaqueIdentity<'a> {
    fn identifiers(&self) -> Identifiers<'a> {
        Identifiers {
            client: Some(self.user),
            server: Some(self.server),
        }
    }
}

fn ksf(params: &KdfParams) -> Result<Argon2<'static>> {
    params.validate()?;
    let params = Params::new(
        params.memory_kib,
        params.iterations,
        params.parallelism,
        None,
    )
    .map_err(|_| CryptoError::WeakKdfParams)?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

// ---------------------------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------------------------

/// Client state kept between the two registration round-trips.
pub struct ClientRegistrationState(ClientRegistration<Suite>);

/// Step 1 of registration. Send the returned request to the server.
pub fn client_registration_start(password: &[u8]) -> Result<(ClientRegistrationState, Vec<u8>)> {
    let start = ClientRegistration::<Suite>::start(&mut OsRng, password)
        .map_err(|_| CryptoError::Opaque)?;
    Ok((
        ClientRegistrationState(start.state),
        start.message.serialize().to_vec(),
    ))
}

impl ClientRegistrationState {
    /// Step 3 of registration. Returns the upload for the server and the master KEK.
    pub fn finish(
        self,
        password: &[u8],
        server_response: &[u8],
        identity: &OpaqueIdentity<'_>,
        params: &KdfParams,
    ) -> Result<(Vec<u8>, SymmetricKey)> {
        let ksf = ksf(params)?;
        let response = RegistrationResponse::<Suite>::deserialize(server_response)
            .map_err(|_| CryptoError::Malformed("registration response"))?;
        let finish = self
            .0
            .finish(
                &mut OsRng,
                password,
                response,
                ClientRegistrationFinishParameters::new(identity.identifiers(), Some(&ksf)),
            )
            .map_err(|_| CryptoError::Opaque)?;
        let kek = master_kek(&finish.export_key)?;
        Ok((finish.message.serialize().to_vec(), kek))
    }
}

/// Client state kept between the two login round-trips.
pub struct ClientLoginState(ClientLogin<Suite>);

/// What the client gets from a successful login.
pub struct ClientLoginOutput {
    /// Send to the server to complete the login.
    pub finalization: Vec<u8>,
    /// Shared secret with the server, for binding the session (e.g. deriving a token MAC key).
    pub session_key: Zeroizing<Vec<u8>>,
    /// Unwraps the account key.
    pub master_kek: SymmetricKey,
}

/// Step 1 of login. Send the returned request to the server.
pub fn client_login_start(password: &[u8]) -> Result<(ClientLoginState, Vec<u8>)> {
    let start =
        ClientLogin::<Suite>::start(&mut OsRng, password).map_err(|_| CryptoError::Opaque)?;
    Ok((
        ClientLoginState(start.state),
        start.message.serialize().to_vec(),
    ))
}

impl ClientLoginState {
    /// Step 3 of login. Fails with [`CryptoError::Opaque`] on a wrong password, an unknown user
    /// or a server that does not hold the user's password file.
    pub fn finish(
        self,
        password: &[u8],
        server_response: &[u8],
        identity: &OpaqueIdentity<'_>,
        params: &KdfParams,
    ) -> Result<ClientLoginOutput> {
        let ksf = ksf(params)?;
        let response = CredentialResponse::<Suite>::deserialize(server_response)
            .map_err(|_| CryptoError::Malformed("credential response"))?;
        let finish = self
            .0
            .finish(
                &mut OsRng,
                password,
                response,
                ClientLoginFinishParameters::new(None, identity.identifiers(), Some(&ksf)),
            )
            .map_err(|_| CryptoError::Opaque)?;
        Ok(ClientLoginOutput {
            finalization: finish.message.serialize().to_vec(),
            session_key: Zeroizing::new(finish.session_key.to_vec()),
            master_kek: master_kek(&finish.export_key)?,
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------------------------

#[cfg(feature = "server")]
pub use server::*;

#[cfg(feature = "server")]
mod server {
    use opaque_ke::rand::rngs::OsRng;
    use opaque_ke::{
        CredentialFinalization, CredentialRequest, RegistrationRequest, RegistrationUpload,
        ServerLogin, ServerLoginParameters, ServerRegistration,
    };
    use zeroize::Zeroizing;

    use super::{OpaqueIdentity, Suite};
    use crate::{CryptoError, Result};

    /// The agent's long-term OPAQUE secret. Generate once, store encrypted at rest, never rotate
    /// casually: rotating it invalidates every password file.
    pub struct ServerSetup(opaque_ke::ServerSetup<Suite>);

    impl ServerSetup {
        pub fn generate() -> Self {
            Self(opaque_ke::ServerSetup::<Suite>::new(&mut OsRng))
        }

        pub fn to_bytes(&self) -> Zeroizing<Vec<u8>> {
            Zeroizing::new(self.0.serialize().to_vec())
        }

        pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
            opaque_ke::ServerSetup::<Suite>::deserialize(bytes)
                .map(Self)
                .map_err(|_| CryptoError::Malformed("server setup"))
        }
    }

    /// Step 2 of registration. `credential_id` must be stable per user (e.g. the user id).
    pub fn server_registration_start(
        setup: &ServerSetup,
        request: &[u8],
        credential_id: &[u8],
    ) -> Result<Vec<u8>> {
        let request = RegistrationRequest::<Suite>::deserialize(request)
            .map_err(|_| CryptoError::Malformed("registration request"))?;
        let start = ServerRegistration::<Suite>::start(&setup.0, request, credential_id)
            .map_err(|_| CryptoError::Opaque)?;
        Ok(start.message.serialize().to_vec())
    }

    /// Step 4 of registration. Returns the password file to store for the user.
    pub fn server_registration_finish(upload: &[u8]) -> Result<Vec<u8>> {
        let upload = RegistrationUpload::<Suite>::deserialize(upload)
            .map_err(|_| CryptoError::Malformed("registration upload"))?;
        Ok(ServerRegistration::<Suite>::finish(upload)
            .serialize()
            .to_vec())
    }

    /// Server state kept between the two login round-trips.
    pub struct ServerLoginState(ServerLogin<Suite>);

    /// Step 2 of login. Pass `password_file: None` for unknown users: OPAQUE then answers with a
    /// fake response, so the endpoint does not reveal which accounts exist.
    pub fn server_login_start(
        setup: &ServerSetup,
        password_file: Option<&[u8]>,
        request: &[u8],
        credential_id: &[u8],
        identity: &OpaqueIdentity<'_>,
    ) -> Result<(ServerLoginState, Vec<u8>)> {
        let password_file = password_file
            .map(ServerRegistration::<Suite>::deserialize)
            .transpose()
            .map_err(|_| CryptoError::Malformed("password file"))?;
        let request = CredentialRequest::<Suite>::deserialize(request)
            .map_err(|_| CryptoError::Malformed("credential request"))?;
        let start = ServerLogin::start(
            &mut OsRng,
            &setup.0,
            password_file,
            request,
            credential_id,
            ServerLoginParameters {
                context: None,
                identifiers: identity.identifiers(),
            },
        )
        .map_err(|_| CryptoError::Opaque)?;
        Ok((
            ServerLoginState(start.state),
            start.message.serialize().to_vec(),
        ))
    }

    impl ServerLoginState {
        /// Step 4 of login. Success proves the client knew the password; returns the session key.
        pub fn finish(
            self,
            finalization: &[u8],
            identity: &OpaqueIdentity<'_>,
        ) -> Result<Zeroizing<Vec<u8>>> {
            let finalization = CredentialFinalization::<Suite>::deserialize(finalization)
                .map_err(|_| CryptoError::Malformed("credential finalization"))?;
            let finish = self
                .0
                .finish(
                    finalization,
                    ServerLoginParameters {
                        context: None,
                        identifiers: identity.identifiers(),
                    },
                )
                .map_err(|_| CryptoError::Opaque)?;
            Ok(Zeroizing::new(finish.session_key.to_vec()))
        }
    }
}
