package com.example.jms.mapper;

import com.example.jms.model.Order;
import org.apache.ibatis.annotations.Param;

import java.util.List;

public interface OrderMapper {

    void dropOrderTable();

    void createOrderTable();

    int insertOrder(Order order);

    Order findById(@Param("id") Long id);

    List<Order> findAllOrders();
}
