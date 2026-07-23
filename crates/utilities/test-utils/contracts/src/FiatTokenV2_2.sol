// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.0;

/// @title FiatTokenV2_2 (devnet fixture)
/// @notice A self-contained, behaviorally compatible ^0.8 port of Circle's `FiatTokenV2_2` (mainnet USDC
///         implementation, originally `circlefin/stablecoin-evm`, Apache-2.0, solc 0.6.12), reduced
///         to the surface the x402 batch-settlement load test exercises.
///
/// @dev Purpose: approximate mainnet-USDC deposit-path gas on devnet. It reproduces the
///      gas-relevant work of the real
///      token on the `deposit`/`receiveWithAuthorization` path:
///        - real EIP-712 + ECDSA signature verification (`ecrecover`),
///        - the ERC-3009 authorization-state `SLOAD`/`SSTORE`,
///        - the blacklist `SLOAD`s (payer + recipient) and the pause `SLOAD`,
///        - EIP-712 domain `name = "USD Coin"`, `version = "2"` (matches USDC v2.2).
///      It is intentionally NOT deployed behind Circle's `FiatTokenProxy`; a devnet fixture omits the
///      upgradeability delegatecall (~one warm `delegatecall`), which is negligible for the deposit
///      gas comparison. All balances/allowances/mint machinery are standard.
contract FiatTokenV2_2 {
    // === ERC-20 metadata ===
    string public name;
    string public symbol;
    uint8 public decimals;

    // === ERC-20 state ===
    uint256 public totalSupply;
    mapping(address => uint256) public balances;
    mapping(address => mapping(address => uint256)) public allowed;

    // === Roles ===
    address public masterMinter;
    address public owner;
    address public pauser;
    address public blacklister;
    mapping(address => bool) internal minters;
    mapping(address => uint256) internal minterAllowed;

    // === Compliance state (gas-relevant SLOADs on transfer/mint) ===
    bool public paused;
    mapping(address => bool) internal blacklisted;

    // === EIP-712 / EIP-3009 ===
    bytes32 public DOMAIN_SEPARATOR;

    bytes32 public constant RECEIVE_WITH_AUTHORIZATION_TYPEHASH = keccak256(
        "ReceiveWithAuthorization(address from,address to,uint256 value,uint256 validAfter,uint256 validBefore,bytes32 nonce)"
    );
    bytes32 public constant TRANSFER_WITH_AUTHORIZATION_TYPEHASH = keccak256(
        "TransferWithAuthorization(address from,address to,uint256 value,uint256 validAfter,uint256 validBefore,bytes32 nonce)"
    );
    bytes32 public constant CANCEL_AUTHORIZATION_TYPEHASH =
        keccak256("CancelAuthorization(address authorizer,bytes32 nonce)");

    /// @dev authorizer => nonce => used.
    mapping(address => mapping(bytes32 => bool)) private _authorizationStates;

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);
    event Mint(address indexed minter, address indexed to, uint256 amount);
    event MinterConfigured(address indexed minter, uint256 minterAllowedAmount);
    event AuthorizationUsed(address indexed authorizer, bytes32 indexed nonce);
    event AuthorizationCanceled(address indexed authorizer, bytes32 indexed nonce);
    event Blacklisted(address indexed account);
    event Pause();
    event Unpause();

    error NotMasterMinter();
    error NotMinter();
    error NotOwner();
    error ExceedsMinterAllowance();
    error ContractPaused();
    error AccountBlacklisted();
    error InvalidAuthorization();
    error AuthorizationUsedOrCanceled();
    error AuthorizationExpired();
    error AuthorizationNotYetValid();
    error CallerMustBePayee();
    error InsufficientBalance();
    error InsufficientAllowance();

    constructor() {
        // Mirrors USDC v2.2 metadata and EIP-712 domain so the token domain separator matches the
        // one the load tester signs against (name "USD Coin", version "2").
        name = "USD Coin";
        symbol = "USDC";
        decimals = 6;
        owner = msg.sender;
        masterMinter = msg.sender;
        pauser = msg.sender;
        blacklister = msg.sender;
        DOMAIN_SEPARATOR = keccak256(
            abi.encode(
                keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"),
                keccak256(bytes(name)),
                keccak256(bytes("2")),
                block.chainid,
                address(this)
            )
        );
    }

    modifier whenNotPaused() {
        if (paused) revert ContractPaused();
        _;
    }

    modifier notBlacklisted(address account) {
        if (blacklisted[account]) revert AccountBlacklisted();
        _;
    }

    // === Roles / minting ===

    function configureMinter(address minter, uint256 minterAllowedAmount) external returns (bool) {
        if (msg.sender != masterMinter) revert NotMasterMinter();
        minters[minter] = true;
        minterAllowed[minter] = minterAllowedAmount;
        emit MinterConfigured(minter, minterAllowedAmount);
        return true;
    }

    function isMinter(address account) external view returns (bool) {
        return minters[account];
    }

    function minterAllowance(address minter) external view returns (uint256) {
        return minterAllowed[minter];
    }

    function mint(
        address to,
        uint256 amount
    ) external whenNotPaused notBlacklisted(msg.sender) notBlacklisted(to) returns (bool) {
        if (!minters[msg.sender]) revert NotMinter();
        uint256 allowedToMint = minterAllowed[msg.sender];
        if (amount > allowedToMint) revert ExceedsMinterAllowance();
        minterAllowed[msg.sender] = allowedToMint - amount;
        totalSupply += amount;
        balances[to] += amount;
        emit Mint(msg.sender, to, amount);
        emit Transfer(address(0), to, amount);
        return true;
    }

    // === Compliance ===

    function blacklist(address account) external {
        if (msg.sender != blacklister) revert NotOwner();
        blacklisted[account] = true;
        emit Blacklisted(account);
    }

    function isBlacklisted(address account) external view returns (bool) {
        return blacklisted[account];
    }

    function pause() external {
        if (msg.sender != pauser) revert NotOwner();
        paused = true;
        emit Pause();
    }

    function unpause() external {
        if (msg.sender != pauser) revert NotOwner();
        paused = false;
        emit Unpause();
    }

    // === ERC-20 ===

    function balanceOf(address account) external view returns (uint256) {
        return balances[account];
    }

    function allowance(address ownerAddr, address spender) external view returns (uint256) {
        return allowed[ownerAddr][spender];
    }

    function approve(
        address spender,
        uint256 value
    ) external whenNotPaused notBlacklisted(msg.sender) notBlacklisted(spender) returns (bool) {
        allowed[msg.sender][spender] = value;
        emit Approval(msg.sender, spender, value);
        return true;
    }

    function transfer(
        address to,
        uint256 value
    ) external whenNotPaused notBlacklisted(msg.sender) notBlacklisted(to) returns (bool) {
        _transfer(msg.sender, to, value);
        return true;
    }

    function transferFrom(
        address from,
        address to,
        uint256 value
    ) external whenNotPaused notBlacklisted(msg.sender) notBlacklisted(from) notBlacklisted(to) returns (bool) {
        uint256 currentAllowance = allowed[from][msg.sender];
        if (value > currentAllowance) revert InsufficientAllowance();
        allowed[from][msg.sender] = currentAllowance - value;
        _transfer(from, to, value);
        return true;
    }

    function _transfer(address from, address to, uint256 value) internal {
        uint256 fromBalance = balances[from];
        if (value > fromBalance) revert InsufficientBalance();
        balances[from] = fromBalance - value;
        balances[to] += value;
        emit Transfer(from, to, value);
    }

    // === EIP-3009 ===

    /// @notice Receive a transfer with a signed authorization from the payer.
    /// @dev ERC-3009 requires `to == msg.sender`, so only the intended recipient (the deposit
    ///      collector) can pull. Mirrors USDC v2.2: time window, unused-nonce check, EIP-712 ECDSA
    ///      verification, then transfer.
    function receiveWithAuthorization(
        address from,
        address to,
        uint256 value,
        uint256 validAfter,
        uint256 validBefore,
        bytes32 nonce,
        bytes memory signature
    ) external whenNotPaused notBlacklisted(from) notBlacklisted(to) {
        if (to != msg.sender) revert CallerMustBePayee();
        _requireValidAuthorization(from, nonce, validAfter, validBefore);
        _verifyEIP712(
            from,
            keccak256(
                abi.encode(
                    RECEIVE_WITH_AUTHORIZATION_TYPEHASH, from, to, value, validAfter, validBefore, nonce
                )
            ),
            signature
        );
        _authorizationStates[from][nonce] = true;
        emit AuthorizationUsed(from, nonce);
        _transfer(from, to, value);
    }

    /// @notice Execute a transfer with a signed authorization from the payer.
    function transferWithAuthorization(
        address from,
        address to,
        uint256 value,
        uint256 validAfter,
        uint256 validBefore,
        bytes32 nonce,
        bytes memory signature
    ) external whenNotPaused notBlacklisted(from) notBlacklisted(to) {
        _requireValidAuthorization(from, nonce, validAfter, validBefore);
        _verifyEIP712(
            from,
            keccak256(
                abi.encode(
                    TRANSFER_WITH_AUTHORIZATION_TYPEHASH, from, to, value, validAfter, validBefore, nonce
                )
            ),
            signature
        );
        _authorizationStates[from][nonce] = true;
        emit AuthorizationUsed(from, nonce);
        _transfer(from, to, value);
    }

    /// @notice Attempt to cancel an authorization before it is used.
    function cancelAuthorization(address authorizer, bytes32 nonce, bytes memory signature) external {
        if (_authorizationStates[authorizer][nonce]) revert AuthorizationUsedOrCanceled();
        _verifyEIP712(
            authorizer,
            keccak256(abi.encode(CANCEL_AUTHORIZATION_TYPEHASH, authorizer, nonce)),
            signature
        );
        _authorizationStates[authorizer][nonce] = true;
        emit AuthorizationCanceled(authorizer, nonce);
    }

    function authorizationState(address authorizer, bytes32 nonce) external view returns (bool) {
        return _authorizationStates[authorizer][nonce];
    }

    function _requireValidAuthorization(
        address authorizer,
        bytes32 nonce,
        uint256 validAfter,
        uint256 validBefore
    ) internal view {
        if (block.timestamp <= validAfter) revert AuthorizationNotYetValid();
        if (block.timestamp >= validBefore) revert AuthorizationExpired();
        if (_authorizationStates[authorizer][nonce]) revert AuthorizationUsedOrCanceled();
    }

    function _verifyEIP712(address signer, bytes32 structHash, bytes memory signature) internal view {
        bytes32 digest = keccak256(abi.encodePacked("\x19\x01", DOMAIN_SEPARATOR, structHash));
        if (signature.length != 65) revert InvalidAuthorization();
        bytes32 r;
        bytes32 s;
        uint8 v;
        assembly {
            r := mload(add(signature, 0x20))
            s := mload(add(signature, 0x40))
            v := byte(0, mload(add(signature, 0x60)))
        }
        address recovered = ecrecover(digest, v, r, s);
        if (recovered == address(0) || recovered != signer) revert InvalidAuthorization();
    }
}
