package com.example.jms.model;

import java.math.BigDecimal;

public class Order {
    private Long id;
    private String customerName;
    private BigDecimal amount;
    private String status;

    public Order() {}

    public Order(Long id, String customerName, BigDecimal amount, String status) {
        this.id = id;
        this.customerName = customerName;
        this.amount = amount;
        this.status = status;
    }

    public Long getId() { return id; }
    public void setId(Long id) { this.id = id; }

    public String getCustomerName() { return customerName; }
    public void setCustomerName(String customerName) { this.customerName = customerName; }

    public BigDecimal getAmount() { return amount; }
    public void setAmount(BigDecimal amount) { this.amount = amount; }

    public String getStatus() { return status; }
    public void setStatus(String status) { this.status = status; }

    @Override
    public String toString() {
        return "Order{" +
                "id=" + id +
                ", customerName='" + customerName + '\'' +
                ", amount=" + amount +
                ", status='" + status + '\'' +
                '}';
    }
}
