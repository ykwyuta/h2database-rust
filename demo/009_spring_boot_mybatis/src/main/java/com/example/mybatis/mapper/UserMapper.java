package com.example.mybatis.mapper;

import com.example.mybatis.model.User;
import org.apache.ibatis.annotations.Param;
import org.apache.ibatis.session.ResultHandler;

import java.util.List;

public interface UserMapper {

    void createSequence();

    void createUserTable();

    Long getNextUserId();

    int insertUser(User user);

    User findById(@Param("id") Long id);

    List<User> findAll();

    int updateStatus(@Param("id") Long id, @Param("status") String status);

    int deleteById(@Param("id") Long id);

    void fetchAllWithCursor(ResultHandler<User> handler);
}
