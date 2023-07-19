use anyhow::{bail, Context};
use candid::{Nat, Principal};
use futures::StreamExt;
use icrc1_test_env::icrc1::{
    balance_of, metadata, supported_standards, token_decimals, token_name, token_symbol, transfer,
    transfer_fee, LedgerTransaction,
};
use icrc1_test_env::{Account, LedgerEnv, Transfer, TransferError, Value};
use std::future::Future;
use std::pin::Pin;

pub enum Outcome {
    Passed,
    Skipped { reason: String },
}

pub type TestResult = anyhow::Result<Outcome>;

pub struct Test {
    name: String,
    action: Pin<Box<dyn Future<Output = TestResult>>>,
}

pub fn test(name: impl Into<String>, body: impl Future<Output = TestResult> + 'static) -> Test {
    Test {
        name: name.into(),
        action: Box::pin(body),
    }
}

fn lookup<'a, K, V, U>(meta: &'a [(K, V)], key: &U) -> Option<&'a V>
where
    K: PartialEq<U>,
    U: ?Sized,
{
    meta.iter().find_map(|(k, v)| (k == key).then_some(v))
}

fn assert_equal<T: PartialEq + std::fmt::Debug>(lhs: T, rhs: T) -> anyhow::Result<()> {
    if lhs != rhs {
        bail!("{:?} ≠ {:?}", lhs, rhs)
    }
    Ok(())
}

async fn assert_balance(
    ledger: &impl LedgerEnv,
    account: impl Into<Account>,
    expected: impl Into<Nat>,
) -> anyhow::Result<()> {
    let account = account.into();
    let actual = balance_of(ledger, account.clone()).await?;
    let expected = expected.into();

    if expected != actual {
        bail!(
            "Expected the balance of account {:?} to be {}, got {}",
            account,
            expected,
            actual
        )
    }
    Ok(())
}

async fn transfer_or_fail(
    ledger_env: &impl LedgerEnv,
    amount: Nat,
    receiver: Principal,
) -> Result<Nat, TransferError> {
    transfer(ledger_env, Transfer::amount_to(amount.clone(), receiver))
        .await
        .with_context(|| format!("failed to transfer {} tokens to {}", amount, receiver))
        .unwrap()
}

async fn setup_test_account(
    ledger_env: &(impl LedgerEnv + LedgerTransaction + std::clone::Clone),
    amount: Nat,
) -> anyhow::Result<impl LedgerEnv + LedgerTransaction> {
    let balance = balance_of(ledger_env, ledger_env.principal()).await?;
    assert!(balance >= amount.clone() + transfer_fee(ledger_env).await?);
    let receiver_env = ledger_env.fork();
    let receiver = receiver_env.principal();
    assert_balance(&receiver_env, receiver, 0).await?;

    let _tx = transfer_or_fail(ledger_env, amount.clone(), receiver)
        .await
        .unwrap();

    assert_balance(
        &receiver_env,
        Account {
            owner: receiver,
            subaccount: None,
        },
        amount.clone(),
    )
    .await?;
    Ok(receiver_env)
}

/// Checks whether the ledger supports token transfers and handles
/// default sub accounts correctly.
/// Expects the given account to have a balance of at least 2*Transfer_Fee
pub async fn test_transfer(ledger_env: impl LedgerEnv + LedgerTransaction) -> TestResult {
    let p1_env = setup_test_account(&ledger_env, Nat::from(20_000)).await?;
    let p2_env = setup_test_account(&ledger_env, Nat::from(20_000)).await?;
    let transfer_amount = 10_000;
    let balance_p1 = balance_of(&p1_env, p1_env.principal()).await?;
    let balance_p2 = balance_of(&p2_env, p2_env.principal()).await?;

    let _tx = transfer_or_fail(&p1_env, Nat::from(transfer_amount), p2_env.principal()).await;

    assert_balance(
        &p2_env,
        Account {
            owner: p2_env.principal(),
            subaccount: None,
        },
        balance_p2.clone() + Nat::from(transfer_amount),
    )
    .await?;

    assert_balance(&ledger_env, Account{ owner: p2_env.principal(), subaccount: Some([0; 32]) }, balance_p2 + transfer_amount)
        .await
        .context("the ledger does not treat accounts with an empty subaccount as accounts with the default subaccount")?;

    assert_balance(
        &p1_env,
        p1_env.principal(),
        balance_p1 - Nat::from(transfer_amount) - transfer_fee(&p1_env).await?,
    )
    .await?;

    Ok(Outcome::Passed)
}

/// Checks whether the ledger supports token burns.
/// Expects the given account to have a balance of at least 2*Transfer_Fee
pub async fn test_burn(ledger_env: impl LedgerEnv + LedgerTransaction) -> TestResult {
    let burn_amount = Nat::from(10_000);
    let p1_env = setup_test_account(&ledger_env, burn_amount.clone()).await?;

    p1_env
        .burn(burn_amount.clone())
        .await?
        .context("failed to burn amount")?;

    assert_balance(&p1_env, p1_env.principal(), 0).await?;
    Ok(Outcome::Passed)
}

/// Checks whether the ledger metadata entries agree with named methods.
pub async fn test_metadata(ledger: impl LedgerEnv) -> TestResult {
    let mut metadata = metadata(&ledger).await?;
    metadata.sort_by(|l, r| l.0.cmp(&r.0));

    for ((k1, _), (k2, _)) in metadata.iter().zip(metadata.iter().skip(1)) {
        if k1 == k2 {
            bail!("Key {} is duplicated in the metadata", k1);
        }
    }

    if let Some(name) = lookup(&metadata, "icrc1:name") {
        assert_equal(&Value::Text(token_name(&ledger).await?), name)
            .context("icrc1:name metadata entry does not match the icrc1_name endpoint")?;
    }
    if let Some(sym) = lookup(&metadata, "icrc1:symbol") {
        assert_equal(&Value::Text(token_symbol(&ledger).await?), sym)
            .context("icrc1:symol metadata entry does not match the icrc1_symbol endpoint")?;
    }
    if let Some(meta_decimals) = lookup(&metadata, "icrc1:decimals") {
        let decimals = token_decimals(&ledger).await?;
        assert_equal(&Value::Nat(Nat::from(decimals)), meta_decimals)
            .context("icrc1:decimals metadata entry does not match the icrc1_decimals endpoint")?;
    }
    if let Some(fee) = lookup(&metadata, "icrc1:fee") {
        assert_equal(&Value::Nat(transfer_fee(&ledger).await?), fee)
            .context("icrc1:fee metadata entry does not match the icrc1_fee endpoint")?;
    }
    Ok(Outcome::Passed)
}

/// Checks whether the ledger advertizes support for ICRC-1 standard.
pub async fn test_supported_standards(ledger: impl LedgerEnv) -> anyhow::Result<Outcome> {
    let stds = supported_standards(&ledger).await?;
    if !stds.iter().any(|std| std.name == "ICRC-1") {
        bail!("The ledger does not claim support for ICRC-1: {:?}", stds);
    }

    Ok(Outcome::Passed)
}

/// Returns the entire list of tests.
pub fn test_suite(env: impl LedgerEnv + LedgerTransaction + 'static) -> Vec<Test> {
    vec![
        test("basic:transfer", test_transfer(env.clone())),
        test("basic:burn", test_burn(env.clone())),
        test("basic:metadata", test_metadata(env.clone())),
        test("basic:supported_standards", test_supported_standards(env)),
    ]
}

/// Executes the list of tests concurrently and prints results using
/// the TAP protocol (https://testanything.org/).
pub async fn execute_tests(tests: Vec<Test>) -> bool {
    use futures::stream::FuturesOrdered;

    let mut names = Vec::new();
    let mut futures = FuturesOrdered::new();

    for test in tests.into_iter() {
        names.push(test.name);
        futures.push_back(test.action);
    }

    println!("TAP version 14");
    println!("1..{}", futures.len());

    let mut idx = 0;
    let mut success = true;
    while let Some(result) = futures.next().await {
        match result {
            Ok(Outcome::Passed) => {
                println!("ok {} - {}", idx + 1, names[idx]);
            }
            Ok(Outcome::Skipped { reason }) => {
                println!("ok {} - {} # SKIP {}", idx + 1, names[idx], reason);
            }
            Err(err) => {
                success = false;

                for line in format!("{:?}", err).lines() {
                    println!("# {}", line);
                }

                println!("not ok {} - {}", idx + 1, names[idx]);
            }
        }
        idx += 1;
    }

    success
}
