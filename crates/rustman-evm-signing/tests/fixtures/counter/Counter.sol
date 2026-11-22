// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

contract Counter {
    uint256 public number;

    event Added(address indexed by, uint256 amount, uint256 number);

    error TooBig(uint256 amount);

    function add(uint256 amount) external returns (uint256) {
        if (amount > 100) revert TooBig(amount);
        number += amount;
        emit Added(msg.sender, amount, number);
        return number;
    }
}
