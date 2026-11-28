package com.shop;

import java.util.List;

public class OrderService {
    private final OrderRepository repository;

    public OrderService(OrderRepository repository) {
        this.repository = repository;
    }

    public List<Order> listMine(long currentUserId) {
        return repository.findByOwner(currentUserId);
    }
}
