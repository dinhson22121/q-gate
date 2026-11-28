package com.shop;

import java.sql.Connection;

public class UserRepository {
    private final Connection connection;

    public UserRepository(Connection connection) {
        this.connection = connection;
    }
}
