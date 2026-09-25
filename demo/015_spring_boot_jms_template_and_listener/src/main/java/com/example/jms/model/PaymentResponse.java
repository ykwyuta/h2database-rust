package com.example.jms.model;

public class PaymentResponse {
    private String paymentId;
    private String status;
    private String transactionCode;
    private String message;

    public PaymentResponse() {
    }

    public PaymentResponse(String paymentId, String status, String transactionCode, String message) {
        this.paymentId = paymentId;
        this.status = status;
        this.transactionCode = transactionCode;
        this.message = message;
    }

    public String getPaymentId() {
        return paymentId;
    }

    public void setPaymentId(String paymentId) {
        this.paymentId = paymentId;
    }

    public String getStatus() {
        return status;
    }

    public void setStatus(String status) {
        this.status = status;
    }

    public String getTransactionCode() {
        return transactionCode;
    }

    public void setTransactionCode(String transactionCode) {
        this.transactionCode = transactionCode;
    }

    public String getMessage() {
        return message;
    }

    public void setMessage(String message) {
        this.message = message;
    }

    @Override
    public String toString() {
        return "PaymentResponse{" +
                "paymentId='" + paymentId + '\'' +
                ", status='" + status + '\'' +
                ", transactionCode='" + transactionCode + '\'' +
                ", message='" + message + '\'' +
                '}';
    }
}
