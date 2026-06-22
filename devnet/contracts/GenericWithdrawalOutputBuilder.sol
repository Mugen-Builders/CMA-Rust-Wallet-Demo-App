// (c) Cartesi and individual authors (see AUTHORS)
// SPDX-License-Identifier: Apache-2.0 (see LICENSE)

pragma solidity ^0.8.8;

import {IERC20} from "@openzeppelin-contracts-5.5.0/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin-contracts-5.5.0/token/ERC20/utils/SafeERC20.sol";
import {IERC721} from "@openzeppelin-contracts-5.5.0/token/ERC721/IERC721.sol";
import {IERC1155} from "@openzeppelin-contracts-5.5.0/token/ERC1155/IERC1155.sol";

interface IWithdrawalOutputBuilder {
    /// @notice Build an output that, when executed by the application
    /// contract, transfers the funds of an account to its owner.
    /// The encoding of the account is application-specific.
    /// This function will be called via the `STATICCALL` opcode,
    /// so any state changes such as contract creations,
    /// log emissions, storage writes, self-destructions
    /// and Ether transfers will revert the call and abort the execution
    /// of the withdrawal output. These state-changing constraints
    /// are already checked by the Solidity compiler when implementing
    /// this function as either view or pure.
    function buildWithdrawalOutput(address appContract, bytes calldata account)
        external
        view
        returns (bytes memory output);
}

interface Outputs {

    /// @notice A single-use permission to execute a specific message call
    /// from the context of the application contract.
    /// @param destination The address that will be called
    /// @param value The amount of Wei to be transferred through the call
    /// @param payload The payload, which—in the case of Solidity
    /// contracts—encodes a function call
    function Voucher(address destination, uint256 value, bytes calldata payload) external;

    /// @notice A single-use permission to execute a specific delegate call
    /// from the context of the application contract.
    /// @param destination The address that will be called
    /// @param payload The payload, which—in the case of Solidity
    /// libraries—encodes a function call
    function DelegateCallVoucher(address destination, bytes calldata payload) external;
}


interface ISafeTokenTransfer {
    /// @notice Safely transfer ERC-20 tokens.
    /// @param token The ERC-20 token contract
    /// @param to The token receipient address
    /// @param value The amount of tokens
    function safeERC20Transfer(IERC20 token, address to, uint256 value) external;

    /// @notice Safely transfer ERC-721 tokens.
    /// @param token The ERC-721 token contract
    /// @param to The token receipient address
    /// @param tokenId The id of the token to transfer
    function safeERC721Transfer(IERC721 token, address from, address to, uint256 tokenId) external;

    /// @notice Safely transfer ERC-1155 tokens.
    /// @param token The ERC-1155 token contract
    /// @param to The token receipient address
    /// @param id The token ID
    /// @param value The amount of tokens
    function safeERC1155Transfer(IERC1155 token, address from, address to, uint256 id, uint256 value) external;
}

contract GenericWithdrawalOutputBuilder is IWithdrawalOutputBuilder, ISafeTokenTransfer {
    using SafeERC20 for IERC20;

    /// @notice Build an output that, when executed by the application
    /// contract, transfers the funds of an account to its owner.
    /// The encoding of the account is application-specific.
    /// This function will be called via the `STATICCALL` opcode,
    /// so any state changes such as contract creations,
    /// log emissions, storage writes, self-destructions
    /// and Ether transfers will revert the call and abort the execution
    /// of the withdrawal output. These state-changing constraints
    /// are already checked by the Solidity compiler when implementing
    /// this function as either view or pure.
    function buildWithdrawalOutput(address appContract, bytes calldata account)
        external
        view
        override
        returns (bytes memory output)
    {
        (uint32 accountType, address user, address tokenAddress, uint256 tokenId, uint256 amount) = _decodeAccount(account);

        if (accountType == 1) {
            return _encodeVoucher(user, amount, "");
        }
        if (accountType == 2) {
            return _encodeDelegateCallVoucher(
                address(this),
                abi.encodeCall(ISafeTokenTransfer.safeERC20Transfer, (IERC20(tokenAddress), user, amount))
            );
        }
        if (accountType == 3) {
            require(amount == 1, "ERC-721 balance must be 1");
            return _encodeDelegateCallVoucher(
                address(this),
                abi.encodeCall(ISafeTokenTransfer.safeERC721Transfer, (IERC721(tokenAddress), appContract, user, tokenId))
            );
        }
        if (accountType == 4) {
            return _encodeDelegateCallVoucher(
                address(this),
                abi.encodeCall(ISafeTokenTransfer.safeERC1155Transfer, (IERC1155(tokenAddress), appContract, user, tokenId, amount))
            );
        }
        revert("Unsupported account type");
    }

    function _decodeAccount(bytes calldata account)
        internal
        pure
        returns (uint8 accountType, address user, address tokenAddress, uint256 tokenId, uint256 balance)
    {
        require(account.length >= 108, "Account is too short");
        
        for (uint256 i; i < 4; ++i) {
            accountType |= uint8(uint256(uint8(account[i])) << (8 * i));
        }
        
        user = address(uint160(bytes20(account[4:24])));
        tokenAddress = address(uint160(bytes20(account[24:44])));
        tokenId = uint256(bytes32(account[44:76]));
        balance = uint256(bytes32(account[76:108]));
    }

    function _encodeVoucher(address destination, uint256 value, bytes memory payload)
        internal
        pure
        returns (bytes memory output)
    {
        return abi.encodeCall(Outputs.Voucher, (destination, value, payload));
    }

    function _encodeDelegateCallVoucher(address destination, bytes memory payload)
        internal
        pure
        returns (bytes memory output)
    {
        return abi.encodeCall(Outputs.DelegateCallVoucher, (destination, payload));
    }

    /// @notice Safely transfer ERC-20 tokens.
    /// @param token The ERC-20 token contract
    /// @param to The token receipient address
    /// @param value The amount of tokens
    function safeERC20Transfer(IERC20 token, address to, uint256 value) external override {
        token.safeTransfer(to, value);
    }

    /// @notice Safely transfer ERC-721 tokens.
    /// @param token The ERC-721 token contract
    /// @param to The token receipient address
    /// @param tokenId The id of the token to transfer
    function safeERC721Transfer(IERC721 token, address from, address to, uint256 tokenId) external override {
        token.safeTransferFrom(from, to, tokenId);
    }

    /// @notice Safely transfer ERC-1155 tokens.
    /// @param token The ERC-1155 token contract
    /// @param to The token receipient address
    /// @param id The token ID
    /// @param value The amount of tokens
    function safeERC1155Transfer(IERC1155 token, address from, address to, uint256 id, uint256 value) external override {
        token.safeTransferFrom(from, to, id, value, "");
    }

}