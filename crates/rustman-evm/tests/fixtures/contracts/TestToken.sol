// SPDX-License-Identifier: MIT
// A minimal ERC-20 for the Anvil integration tests of rustman-evm. The
// tests compile it with `forge build --offline` (solc 0.8.26) and deploy
// it with `forge create`.
pragma solidity 0.8.26;

contract TestToken {
    error InsufficientBalance(uint256 available, uint256 required);

    event Transfer(address indexed from, address indexed to, uint256 value);

    string public constant name = "Test Token";
    string public constant symbol = "TST";
    uint8 public constant decimals = 6;
    uint256 public totalSupply;
    mapping(address => uint256) public balanceOf;

    constructor(uint256 supply) {
        totalSupply = supply;
        balanceOf[msg.sender] = supply;
        emit Transfer(address(0), msg.sender, supply);
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        uint256 available = balanceOf[msg.sender];
        if (available < amount) {
            revert InsufficientBalance(available, amount);
        }
        balanceOf[msg.sender] = available - amount;
        balanceOf[to] += amount;
        emit Transfer(msg.sender, to, amount);
        return true;
    }

    /// Reads the balance through an external call to itself, so a call
    /// trace has a nested frame.
    function balanceViaSelf(address who) external view returns (uint256) {
        return this.balanceOf(who);
    }

    function ratio(uint256 a, uint256 b) external pure returns (uint256) {
        return a / b;
    }

    function check(uint256 value) external pure returns (uint256) {
        require(value < 100, "value too large");
        return value;
    }
}
