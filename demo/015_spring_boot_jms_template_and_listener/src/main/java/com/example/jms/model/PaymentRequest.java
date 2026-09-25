package com.example.jms.model;

public class PaymentRequest {
    private String paymentId;
    private String customer;
    private double amount;
    private String currency;

    public PaymentRequest() {
    }

    public PaymentRequest(String paymentId, String customer, double amount, String currency) {
        this.paymentId = paymentId;
        this.customer = customer;
        this.amount = amount;
        this.currency = currency;
    }

    public String getPaymentId() {
        return paymentId;
    }

    public void setPaymentId(String paymentId) {
        this.paymentId = paymentId;
    }

    public String getCustomer() {
        return customer;
    }

    public void setCustomer(String customer) {
        this.customer = customer;
    }

    public double getAmount() {
        return amount;
    }

    public void setAmount(double amount) {
        this.amount = amount;
    }

    public String getCurrency() {
        return currency;
    }

    public void setCurrency(String currency) {
        this.currency = currency;
    }

    @Override
    public String toString() {
        return "PaymentRequest{" +
                "paymentId='" + paymentId + '\'' +
                ", customer='" + customer + '\'' +
                ", amount=" + amount +
                ", currency='" + currency + '\'' +
                '}';
    }
}
