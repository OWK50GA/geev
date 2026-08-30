use crate::profile::{ProfileContract, SLASH_AMOUNT};
use crate::types::{DataKey, Error, GiveawayStatus, HelpRequestStatus};
use crate::utils::validate_fee;
use crate::{access::check_admin, types::HelpRequest};
use soroban_sdk::{contract, contractevent, contractimpl, panic_with_error, token, Address, Env};

#[contract]
pub struct AdminContract;

#[contractevent]
pub struct EmergencyWithdraw {
    token: Address,
    amount: i128,
    to: Address,
}

#[contractevent]
pub struct TokenAdded {
    token: Address,
}

#[contractevent]
pub struct TokenRemoved {
    token: Address,
}

#[contractevent]
pub struct RequestVerificationChanged {
    request_id: u64,
    is_verified: bool,
}

#[contractevent]
pub struct AppealResolved {
    #[topic]
    target_id: u64,
    restored: bool,
}

/// Emitted when admin control is transferred. Topics are `admin`, `transfer`,
/// plus the previous admin address; data is `[new_admin]`.
#[contractevent(topics = ["admin", "transfer"], data_format = "vec")]
pub struct AdminTransferred {
    #[topic]
    previous_admin: Address,
    new_admin: Address,
}

/// Emitted when the contract is paused by an admin.
#[contractevent]
pub struct ContractPaused {
    admin: Address,
}

/// Emitted when the contract is unpaused by an admin.
#[contractevent]
pub struct ContractUnpaused {
    admin: Address,
}

#[contractimpl]
impl AdminContract {
    /// Emergency withdraw function - callable only by Admin
    /// Allows rescuing funds in case of critical bugs, exploits, or migration needs
    ///
    /// # Arguments
    /// * `env` - The contract environment
    /// * `token` - The token address to withdraw
    /// * `amount` - The amount to withdraw
    /// * `to` - The safe address to send funds to
    ///
    /// # Panics
    /// Panics if called by non-admin address
    pub fn admin_withdraw(env: Env, token: Address, amount: i128, to: Address) {
        // Check admin authentication
        check_admin(&env);

        // Initialize Token Client
        let token_client = token::Client::new(&env, &token);

        // Execute transfer: From contract -> to
        token_client.transfer(&env.current_contract_address(), &to, &amount);

        // Emit EmergencyWithdraw event
        EmergencyWithdraw {
            token: token.clone(),
            amount,
            to: to.clone(),
        }
        .publish(&env);
    }

    /// Add a token to the whitelist - callable only by Admin
    /// Allows specific tokens to be used for giveaway creation
    ///
    /// # Arguments
    /// * `env` - The contract environment
    /// * `token` - The token address to whitelist
    ///
    /// # Panics
    /// Panics if called by non-admin address
    pub fn add_token(env: Env, token: Address) {
        // Check admin authentication
        check_admin(&env);

        // Add token to whitelist
        let token_key = DataKey::AllowedToken(token.clone());
        env.storage().instance().set(&token_key, &true);

        // Emit TokenAdded event
        TokenAdded { token }.publish(&env);
    }

    /// Remove a token from the whitelist - callable only by Admin.
    ///
    /// Delisting only blocks **new** giveaway creation: `create_giveaway` rejects
    /// tokens that are not allowlisted (`Error::TokenNotSupported`).
    ///
    /// Previously funded records remain fully supported after delisting:
    /// existing giveaways can still accept entries, select winners, and
    /// distribute prizes; existing help requests can still receive donations
    /// and process refunds. Lifecycle of in-flight escrow is unchanged.
    ///
    /// # Arguments
    /// * `env` - The contract environment
    /// * `token` - The token address to delist
    ///
    /// # Panics
    /// Panics if called by non-admin address
    pub fn remove_token(env: Env, token: Address) {
        check_admin(&env);

        let token_key = DataKey::AllowedToken(token.clone());
        env.storage().instance().set(&token_key, &false);

        TokenRemoved { token }.publish(&env);
    }

    pub fn toggle_request_verification(env: Env, request_id: u64) {
        check_admin(&env);

        let request_key = DataKey::HelpRequest(request_id);
        let mut request: HelpRequest = env
            .storage()
            .persistent()
            .get(&request_key)
            .unwrap_or_else(|| panic_with_error!(&env, Error::HelpRequestNotFound));

        request.is_verified = !request.is_verified;

        env.storage().persistent().set(&request_key, &request);
        RequestVerificationChanged {
            request_id,
            is_verified: request.is_verified,
        }
        .publish(&env);
    }

    /// Resolve an appeal for suspended content - callable only by Admin
    /// Allows admin to restore the content or keep it suspended.
    /// When `restore` is true, the author's reputation is credited back by
    /// [`SLASH_AMOUNT`] (reversing the auto-suspend slash).
    pub fn resolve_appeal(env: Env, target_id: u64, restore: bool) {
        check_admin(&env);

        let giveaway_key = DataKey::Giveaway(target_id);
        let request_key = DataKey::HelpRequest(target_id);

        let mut resolved = false;
        let mut author_to_restore: Option<Address> = None;

        // Try Giveaway first.
        if let Some(mut giveaway) = env
            .storage()
            .persistent()
            .get::<DataKey, crate::types::Giveaway>(&giveaway_key)
        {
            if giveaway.status == GiveawayStatus::UnderAppeal {
                if restore {
                    giveaway.status = GiveawayStatus::Active;
                    author_to_restore = Some(giveaway.creator.clone());
                } else {
                    giveaway.status = GiveawayStatus::Suspended;
                }
                env.storage().persistent().set(&giveaway_key, &giveaway);
                resolved = true;
            }
        }

        // Try HelpRequest if giveaway wasn't found/resolved.
        if !resolved {
            if let Some(mut request) = env
                .storage()
                .persistent()
                .get::<DataKey, crate::types::HelpRequest>(&request_key)
            {
                if request.status == HelpRequestStatus::UnderAppeal {
                    if restore {
                        request.status = HelpRequestStatus::Open;
                        author_to_restore = Some(request.creator.clone());
                    } else {
                        request.status = HelpRequestStatus::Suspended;
                    }
                    env.storage().persistent().set(&request_key, &request);
                    resolved = true;
                }
            }
        }

        if resolved {
            if let Some(author) = author_to_restore {
                ProfileContract::restore_reputation(&env, author, SLASH_AMOUNT);
            }
            AppealResolved {
                target_id,
                restored: restore,
            }
            .publish(&env);
        }
    }

    /// Transfer admin role to a new address - callable only by the current Admin
    ///
    /// # Arguments
    /// * `env` - The contract environment
    /// * `current_admin` - The address of the current admin (must match stored admin)
    /// * `new_admin` - The address that will become the new admin
    ///
    /// # Panics
    /// Panics if called by a non-admin address or if `current_admin` does not match storage
    pub fn transfer_admin(env: Env, current_admin: Address, new_admin: Address) {
        let admin = check_admin(&env);

        if admin != current_admin {
            panic_with_error!(&env, Error::NotAdmin);
        }

        env.storage().instance().set(&DataKey::Admin, &new_admin);

        AdminTransferred {
            previous_admin: current_admin,
            new_admin,
        }
        .publish(&env);
    }

    /// Update the global protocol fee (basis points) after init.
    ///
    /// Does not rewrite already-accrued `CollectedFees`. Effective fee at claim
    /// time still follows: giveaway override → token fee → global fee → default.
    ///
    /// # Panics
    /// Panics if caller is not admin or `fee_bps` exceeds `MAX_FEE_BPS`.
    pub fn set_fee(env: Env, fee_bps: u32) {
        check_admin(&env);
        validate_fee(&env, fee_bps);
        env.storage().instance().set(&DataKey::Fee, &fee_bps);
    }

    /// Set a per-token fee that takes precedence over the global fee.
    ///
    /// # Panics
    /// Panics if caller is not admin or `fee_bps` exceeds `MAX_FEE_BPS`.
    pub fn set_token_fee(env: Env, token: Address, fee_bps: u32) {
        check_admin(&env);
        validate_fee(&env, fee_bps);
        env.storage()
            .instance()
            .set(&DataKey::TokenFee(token), &fee_bps);
    }

    /// Pause the contract — blocks all value-moving entrypoints.
    ///
    /// Callable only by the admin. Emits [`ContractPaused`] on each
    /// transition. `admin_withdraw` and `unpause` remain callable while
    /// paused.
    ///
    /// # Panics
    /// Panics if called by a non-admin address.
    pub fn pause(env: Env) {
        let admin = check_admin(&env);
        env.storage().instance().set(&DataKey::Paused, &true);
        ContractPaused { admin }.publish(&env);
    }

    /// Unpause the contract — restores all value-moving entrypoints.
    ///
    /// Callable only by the admin, including while the contract is paused.
    /// Emits [`ContractUnpaused`] on each transition.
    ///
    /// # Panics
    /// Panics if called by a non-admin address.
    pub fn unpause(env: Env) {
        let admin = check_admin(&env);
        env.storage().instance().set(&DataKey::Paused, &false);
        ContractUnpaused { admin }.publish(&env);
    }
}
