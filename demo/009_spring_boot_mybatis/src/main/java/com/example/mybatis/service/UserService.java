package com.example.mybatis.service;

import com.example.mybatis.mapper.AnalyticsMapper;
import com.example.mybatis.mapper.SubscriptionMapper;
import com.example.mybatis.mapper.UserMapper;
import com.example.mybatis.model.AnalyticsMetric;
import com.example.mybatis.model.Subscription;
import com.example.mybatis.model.User;
import org.springframework.stereotype.Service;
import org.springframework.transaction.annotation.Transactional;

import java.util.ArrayList;
import java.util.List;

@Service
public class UserService {

    private final UserMapper userMapper;
    private final AnalyticsMapper analyticsMapper;
    private final SubscriptionMapper subscriptionMapper;

    public UserService(UserMapper userMapper, AnalyticsMapper analyticsMapper, SubscriptionMapper subscriptionMapper) {
        this.userMapper = userMapper;
        this.analyticsMapper = analyticsMapper;
        this.subscriptionMapper = subscriptionMapper;
    }

    public void initializeSchema() {
        userMapper.createSequence();
        userMapper.createUserTable();
        analyticsMapper.createAnalyticsTable();
        subscriptionMapper.createSubscriptionTable();
    }

    /**
     * Primary (Read-Write) への書き込みトランザクション
     */
    @Transactional
    public User registerUser(String username, String email) {
        Long nextId = userMapper.getNextUserId();
        User user = new User(nextId, username, email, "ACTIVE");
        userMapper.insertUser(user);
        return user;
    }

    /**
     * Standby (Read-Only) への自動ルーティング検証 (readOnly = true)
     */
    @Transactional(readOnly = true)
    public User getUserFromStandby(Long id) {
        return userMapper.findById(id);
    }

    /**
     * Standby (Read-Only) からの全件取得
     */
    @Transactional(readOnly = true)
    public List<User> getAllUsersFromStandby() {
        return userMapper.findAll();
    }

    /**
     * トランザクションロールバックの実演
     */
    @Transactional
    public void registerUserAndRollback(String username, String email) {
        Long nextId = userMapper.getNextUserId();
        User user = new User(nextId, username, email, "PENDING_ROLLBACK");
        userMapper.insertUser(user);
        // 意図的に例外をスローしてロールバックを誘発
        throw new IllegalStateException("Intentional business failure to trigger transaction rollback!");
    }

    @Transactional
    public void recordMetric(String name, Double val) {
        analyticsMapper.insertMetric(name, val);
    }

    @Transactional(readOnly = true)
    public List<AnalyticsMetric> getCalculatedMetrics() {
        return analyticsMapper.calculateAdvancedMetrics();
    }

    @Transactional(readOnly = true)
    public List<AnalyticsMetric> searchMetricsByRegex(String pattern) {
        return analyticsMapper.searchByRegex(pattern);
    }

    @Transactional
    public void addSubscription(Long userId, String planName) {
        subscriptionMapper.insertSubscription(userId, planName);
    }

    @Transactional(readOnly = true)
    public List<Subscription> getSubscriptions() {
        return subscriptionMapper.findAllSubscriptions();
    }

    @Transactional(readOnly = true)
    public List<User> streamUsersWithCursor() {
        List<User> collected = new ArrayList<>();
        userMapper.fetchAllWithCursor(resultContext -> {
            collected.add(resultContext.getResultObject());
        });
        return collected;
    }
}
